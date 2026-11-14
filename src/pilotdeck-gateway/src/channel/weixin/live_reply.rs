//! Full port of `ImLiveReplyController.ts` (the shared IM live-reply
//! controller). The pr1 weixin subset (the inline `WeixinReplyBuffer`) is
//! replaced by this controller; the transport seam is the TS
//! `ImLiveReplyTransport` shape — optional `edit` / `delete` / `clear` /
//! `pulseActivity` / `stopActivity` / `maxMessageLength` / `formatText`
//! capabilities. Weixin provides only `send` / `pulseActivity` /
//! `stopActivity`, exactly like `WeixinChannel.ts:786-800`: no `edit` and no
//! `maxMessageLength`, so those two paths are pinned by the controller-level
//! mock-transport tests in this file rather than by weixin replay goldens.
//!
//! Timers: TS arms `setTimeout` callbacks (every one `.unref()`ed). This port
//! keeps the four deadlines as `Option<tokio::time::Instant>` and fires them
//! explicitly through [`LiveReplyController::fire_due_timers`], so tests drive
//! them on the paused tokio clock (`start_paused` + `advance`) instead of
//! sleeping, and the weixin wiring selects between the next stream event and
//! [`LiveReplyController::wait_for_timer`].
//!
//! `runExclusive` / `inFlight` / `waitForInFlight` (ImLiveReplyController.ts:
//! 609-627) serialize overlapping flushes across JS microtasks. Here every call
//! path owns `&mut self`, so transport calls are already strictly ordered and
//! the queue degenerates to sequential awaits — no spawned task and no shared
//! handle needed. `void this.append(...)` in `handleAgentStatus` is awaited
//! the same way: the TS fire-and-forget promise only reordered work inside the
//! same microtask queue this port runs sequentially.
//!
//! Not ported: `Segment.final` and `controller.lastFlushAt` — the TS writes
//! both and never reads either (the PR table records this).
//!
//! The API surface is the full TS controller method set; the weixin wiring only
//! uses the transport subset (no `edit`, no `delete`/`clear`, no
//! `resume_activity` until pr2c lands the permission helpers), so several
//! items are exercised only by the tests in this file — hence the module-level
//! dead-code allow instead of per-item sprinkles.
#![allow(dead_code)]
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use pilotdeck_protocol::JsValue;
use tokio::time::Instant;

use super::js_string_text;

/// TS `ImLiveReplyTransport` method future: `Err` is the JS throw, the `Ok`
/// payload keeps the TS trichotomy where it matters.
pub type TransportFuture<T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send>>;

/// The transport seam's optional capability closures (`edit` and `formatText`
/// exist only on full-controller transports, never on weixin's).
pub type TransportEdit<H> = Arc<dyn Fn(H, String) -> TransportFuture<bool> + Send + Sync>;
pub type TransportFormatText = Arc<dyn Fn(&str) -> String + Send + Sync>;
pub type TransportErrorReport = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// TS `send()` resolves `handle | undefined | false`: a handle to edit later,
/// `undefined` for fire-and-forget transports (weixin), `false` for a refused
/// send.
pub enum SendOutcome<H> {
    Handle(H),
    NoHandle,
    Failed,
}

/// TS `ImLiveReplyActivityKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityKind {
    Thinking,
    Tool,
    Subagent,
}
impl ActivityKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Thinking => "thinking",
            Self::Tool => "tool",
            Self::Subagent => "subagent",
        }
    }
}

/// TS `ImLiveReplyActivity`. `text` is what the controller fills from
/// `format_activity` before pulsing (the formatter sees the activity with an
/// empty `text`, matching the TS `Omit<..., "text">` shape).
#[derive(Clone, Debug)]
pub struct LiveReplyActivity {
    pub kind: ActivityKind,
    pub text: String,
    pub elapsed_ms: u64,
    pub update_count: u32,
    pub detail: Option<String>,
}

/// TS `ImLiveReplyTransport`. `send` is the one required method; each optional
/// capability is the presence of its closure (`weixin` supplies only the
/// activity pair).
pub struct LiveReplyTransport<H> {
    pub send: Arc<dyn Fn(String) -> TransportFuture<SendOutcome<H>> + Send + Sync>,
    pub edit: Option<TransportEdit<H>>,
    pub delete: Option<Arc<dyn Fn(H) -> TransportFuture<bool> + Send + Sync>>,
    pub clear: Option<Arc<dyn Fn(H) -> TransportFuture<bool> + Send + Sync>>,
    pub pulse_activity:
        Option<Arc<dyn Fn(LiveReplyActivity) -> TransportFuture<bool> + Send + Sync>>,
    pub stop_activity: Option<Arc<dyn Fn() -> TransportFuture<bool> + Send + Sync>>,
    pub max_message_length: Option<usize>,
    pub format_text: Option<TransportFormatText>,
}

/// TS `ImLiveReplyControllerOptions` defaults
/// (ImLiveReplyController.ts:74-86); weixin overrides live in
/// [`super::weixin_live_reply_options`].
#[derive(Clone)]
pub struct LiveReplyOptions {
    pub throttle_ms: u64,
    pub buffer_threshold: usize,
    pub initial_throttle_ms: u64,
    pub initial_buffer_threshold: usize,
    pub turn_timeout_ms: u64,
    /// Negative keeps the delay timer unarmed (TS `scheduleActivityDelay`
    /// returns before arming).
    pub activity_delay_ms: i64,
    pub activity_update_throttle_ms: u64,
    pub activity_max_updates: u32,
    pub activity_ttl_ms: u64,
    pub cursor: String,
    pub activity_only_final_text: String,
    pub timeout_final_text: String,
    pub abort_final_text: String,
    pub format_activity: Arc<dyn Fn(&LiveReplyActivity) -> String + Send + Sync>,
    pub on_transport_error: Option<TransportErrorReport>,
}
impl Default for LiveReplyOptions {
    fn default() -> Self {
        Self {
            throttle_ms: 2000,
            buffer_threshold: 96,
            initial_throttle_ms: 800,
            initial_buffer_threshold: 24,
            turn_timeout_ms: 600_000,
            activity_delay_ms: 2500,
            activity_update_throttle_ms: 10_000,
            activity_max_updates: 6,
            activity_ttl_ms: 120_000,
            cursor: " ▉".into(),
            activity_only_final_text: "处理完成，但没有可见回复。".into(),
            timeout_final_text: "处理超时，请重新发送或稍后重试。".into(),
            abort_final_text: DEFAULT_ABORT_FINAL_TEXT.into(),
            format_activity: Arc::new(default_activity_formatter),
            on_transport_error: None,
        }
    }
}

/// The result of one [`LiveReplyController::fire_due_timers`] round.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TimerFirings {
    /// The turn timer fired this round (the controller already ran
    /// `mark_timed_out`); the wiring owes the gateway abort.
    pub turn_timed_out: bool,
}

struct Segment<H> {
    text: String,
    handle: Option<H>,
    first_text_at: Option<Instant>,
    last_visible_text: String,
    last_visible_final_text: String,
    edit_disabled: bool,
    fallback_prefix: String,
    activity_kind: ActivityKind,
    activity_started_at: Instant,
    activity_armed: bool,
    activity_updates: u32,
    activity_visible: bool,
    activity_disabled: bool,
    activity_detail: Option<String>,
}
fn create_segment<H>() -> Segment<H> {
    Segment {
        text: String::new(),
        handle: None,
        first_text_at: None,
        last_visible_text: String::new(),
        last_visible_final_text: String::new(),
        edit_disabled: false,
        fallback_prefix: String::new(),
        activity_kind: ActivityKind::Thinking,
        activity_started_at: Instant::now(),
        activity_armed: false,
        activity_updates: 0,
        activity_visible: false,
        activity_disabled: false,
        activity_detail: None,
    }
}

struct FlushParams {
    final_: bool,
    force: bool,
    finalize_activity_only: bool,
}

/// TS `ImLiveReplyController.ts:78` `VISIBLE_FAILURE_STATUS_EVENTS`.
const VISIBLE_FAILURE_STATUS_EVENTS: &[&str] = &[
    "model_empty_response_exhausted",
    "max_turns_reached",
    "max_output_recovery_exhausted",
    "model_request_failed",
    "tool_call_recovery_exhausted",
    "tool_error_loop",
    "lifecycle_blocked",
    "turn_failed",
    "turn_timeout",
    "gateway_submit_failed",
    "session_busy",
    "gateway_bridge_error",
    "gateway_stream_ended_without_completion",
    "web_http_request_failed",
    "project_unavailable",
    "config_invalid",
    "gateway_unavailable",
    "channel_submit_failed",
    "subagent_failed",
    "content_filter_stop",
    "unknown_finish_reason",
];
/// ImLiveReplyController.ts:102-106: `WARNING_STATUS_EVENTS` derives from the
/// visible-failure list plus two entries, so the two can never drift. Both
/// memberships come out of one lookup (#1725): `handle_agent_status` needs
/// the visible-failure list for the failure flag and the warning list for
/// the text append, and the shared list must not be scanned twice per event.
const WARNING_STATUS_EVENTS_EXTRA: &[&str] = &["structured_output_completed", "turn_aborted"];
fn status_event_memberships(name: &str) -> (bool, bool) {
    let visible_failure = VISIBLE_FAILURE_STATUS_EVENTS.contains(&name);
    let warning = visible_failure || WARNING_STATUS_EVENTS_EXTRA.contains(&name);
    (visible_failure, warning)
}

const EMPTY_RESPONSE_EXHAUSTED_TEXT: &str = "The model returned empty content repeatedly, so this turn has stopped. Try again later or increase max output tokens.";
const MAX_TURNS_TEXT: &str = "Reached the maximum number of turns, so this turn has stopped. Increase maxTurns or split the task into smaller steps and try again.";
/// The weixin catch path feeds `channel_submit_failed` with this message
/// (WeixinChannel.ts:753-768).
const WEIXIN_SUBMIT_FAILED_TEXT: &str = "Failed to process this message. Please retry.";
/// TS default `abortFinalText` (weixin passes no override).
const DEFAULT_ABORT_FINAL_TEXT: &str = "处理已中止，请重新发送或稍后重试。";

// ── JS string semantics: lengths and slices count UTF-16 units ──────────────

/// JS `String.prototype.length`.
fn js_len(text: &str) -> usize {
    text.chars().map(|c| c.len_utf16()).sum()
}
/// The byte offset of UTF-16 unit `units`. A surrogate pair the cut lands
/// inside stays whole — a `String` cannot hold the lone half JS would slice
/// out, and no golden straddles one.
fn js_cut(text: &str, units: usize) -> usize {
    let mut taken = 0;
    for (index, c) in text.char_indices() {
        if taken >= units {
            return index;
        }
        taken += c.len_utf16();
    }
    text.len()
}
fn js_prefix(text: &str, units: usize) -> &str {
    &text[..js_cut(text, units)]
}
fn js_suffix(text: &str, units: usize) -> &str {
    &text[js_cut(text, units)..]
}
/// JS `text.lastIndexOf(needle, limit)`: the last match starting at an index
/// `<= limit`, or -1.
fn last_index_of(text: &str, needle: char, limit: usize) -> isize {
    let mut best = -1isize;
    let mut units = 0usize;
    for c in text.chars() {
        if units > limit {
            break;
        }
        if c == needle {
            best = units as isize;
        }
        units += c.len_utf16();
    }
    best
}

/// TS `continuationText` (ImLiveReplyController.ts:977-981).
fn continuation_text(final_text: &str, visible_prefix: &str) -> String {
    if visible_prefix.is_empty() {
        return final_text.to_string();
    }
    if !final_text.starts_with(visible_prefix) {
        return final_text.to_string();
    }
    js_suffix(final_text, js_len(visible_prefix))
        .trim_start_matches(' ')
        .to_string()
}

/// TS `defaultToolErrorFormatter`.
fn format_tool_error(event: &JsValue) -> String {
    let name = event
        .get("toolName")
        .or_else(|| event.get("toolCallId"))
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
        .unwrap_or_default();
    format!("\n⚠️ {name} 执行失败\n")
}

/// TS `defaultErrorFormatter` (ImLiveReplyController.ts:988-1002).
fn format_error(event: &JsValue) -> String {
    let message =
        super::js_template_text(&event.get("message").cloned().unwrap_or(JsValue::Undefined));
    let mut provider_lines = String::new();
    if let Some(provider) = event.get("providerError") {
        let mut lines = vec![String::new(), "Provider error:".into()];
        let text_field = |name: &str, prefix: &str| {
            provider
                .get(name)
                .and_then(JsValue::as_string)
                .filter(|value| !value.units().is_empty())
                .map(|value| format!("{prefix}{}", String::from_utf16_lossy(value.units())))
        };
        if let Some(line) = text_field("provider", "- provider: ") {
            lines.push(line);
        }
        if let Some(status) = provider
            .get("status")
            .filter(|status| !status.is_undefined())
        {
            lines.push(format!("- status: {}", super::js_template_text(status)));
        }
        if let Some(line) = text_field("code", "- code: ") {
            lines.push(line);
        }
        if let Some(line) = text_field("message", "- message: ") {
            lines.push(line);
        }
        if let Some(line) = text_field("raw", "- raw: ") {
            lines.push(line);
        }
        provider_lines = lines.join("\n");
    }
    format!("\n❌ {message}{provider_lines}\n")
}

/// TS `defaultActivityFormatter` (ImLiveReplyController.ts:1004-1014).
fn default_activity_formatter(activity: &LiveReplyActivity) -> String {
    let base = match activity.kind {
        ActivityKind::Tool => "正在执行工具…",
        ActivityKind::Subagent => "正在处理子任务…",
        ActivityKind::Thinking => "正在思考…",
    };
    if activity.elapsed_ms < 10_000 {
        return base.into();
    }
    format!("{base}（{}s）", (activity.elapsed_ms + 500) / 1000)
}

/// `WeixinChannel.ts:1081-1092 formatElapsed`: empty under a minute,
/// whole minutes under an hour, then the hour form.
pub(crate) fn format_elapsed(elapsed_ms: u64) -> String {
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

/// `WeixinChannel.ts:1069-1079 formatWeixinActivity`.
pub(crate) fn format_weixin_activity(activity: &LiveReplyActivity) -> String {
    let elapsed = format_elapsed(activity.elapsed_ms);
    let suffix = if elapsed.is_empty() {
        String::new()
    } else {
        format!("（已用时 {elapsed}）")
    };
    match activity.kind {
        ActivityKind::Tool => format!("仍在处理：正在执行工具{suffix}"),
        ActivityKind::Subagent => format!("仍在处理：正在处理子任务{suffix}"),
        ActivityKind::Thinking => format!("仍在处理：正在分析和生成回复{suffix}"),
    }
}

/// The controller. One per turn, as in TS.
pub struct LiveReplyController<H = ()> {
    transport: LiveReplyTransport<H>,
    options: LiveReplyOptions,
    current_segment: Segment<H>,
    completed_segments: Vec<Segment<H>>,
    text_timer: Option<Instant>,
    turn_timer: Option<Instant>,
    activity_delay_timer: Option<Instant>,
    activity_update_timer: Option<Instant>,
    closed: bool,
    turn_timer_armed: bool,
    has_visible_failure_status: bool,
    timed_out: bool,
}

impl<H: Clone + Send + 'static> LiveReplyController<H> {
    pub fn new(transport: LiveReplyTransport<H>, options: LiveReplyOptions) -> Self {
        Self {
            transport,
            options,
            current_segment: create_segment(),
            completed_segments: Vec::new(),
            text_timer: None,
            turn_timer: None,
            activity_delay_timer: None,
            activity_update_timer: None,
            closed: false,
            turn_timer_armed: false,
            has_visible_failure_status: false,
            timed_out: false,
        }
    }

    // ── public surface (names mirror the TS methods; see the PR table) ─────

    /// TS `handleEvent` (ImLiveReplyController.ts:169-218).
    pub async fn handle_event(&mut self, event: &JsValue) {
        if self.closed {
            return;
        }
        self.arm_turn_timer(false);
        let event_type = js_string_text(event.get("type"));
        match event_type.as_str() {
            "turn_started" => {
                self.has_visible_failure_status = false;
                self.arm_turn_timer(true);
                self.mark_activity(ActivityKind::Thinking, None);
            }
            "model_request_started" => {
                self.mark_activity(ActivityKind::Thinking, None);
            }
            "assistant_thinking_delta" => {
                let text = js_string_text(event.get("text"));
                self.mark_activity(ActivityKind::Thinking, Some(&text));
            }
            "assistant_text_delta" => {
                let text = js_string_text(event.get("text"));
                self.append(&text).await;
            }
            "tool_call_started" => {
                if self.should_flush_before_tool_boundary() {
                    self.flush_segment(false).await;
                } else {
                    self.clear_text_timer();
                }
                let name = js_string_text(event.get("name"));
                self.mark_activity(ActivityKind::Tool, Some(&name));
            }
            "tool_call_finished" => {
                if event.get("ok") == Some(&JsValue::Bool(false)) {
                    let text = format_tool_error(event);
                    self.append(&text).await;
                } else {
                    self.mark_activity(ActivityKind::Thinking, None);
                }
            }
            "agent_status" => self.handle_agent_status(event).await,
            "elicitation_request" => self.pause_activity().await,
            "error" => {
                if self.has_visible_failure_status {
                    return;
                }
                let text = format_error(event);
                self.append(&text).await;
            }
            _ => {}
        }
    }

    /// TS `markTimedOut`: the terminal timeout text replaces the segment.
    pub async fn mark_timed_out(&mut self) {
        self.timed_out = true;
        let text = self.options.timeout_final_text.clone();
        self.finalize_with_terminal_text(&text).await;
    }

    /// TS `markAborted`.
    pub async fn mark_aborted(&mut self) {
        let text = self.options.abort_final_text.clone();
        self.finalize_with_terminal_text(&text).await;
    }

    /// TS `flushFinal`.
    pub async fn flush_final(&mut self) {
        if self.closed {
            return;
        }
        self.clear_turn_timer();
        self.flush_segment(true).await;
        self.closed = true;
        self.stop_native_activity().await;
    }

    /// TS `flushSegment` (the TS default `finalizeActivityOnly` is `true`).
    pub async fn flush_segment(&mut self, finalize_activity_only: bool) {
        self.clear_text_timer();
        self.clear_activity_timers();
        self.flush_current(FlushParams {
            final_: true,
            force: true,
            finalize_activity_only,
        })
        .await;
        let segment = &self.current_segment;
        let keep = !crate::metadata::js_trim(&segment.text).is_empty()
            || !crate::metadata::js_trim(&segment.last_visible_final_text).is_empty()
            || (finalize_activity_only
                && !crate::metadata::js_trim(&segment.last_visible_text).is_empty());
        if keep {
            let segment = std::mem::replace(&mut self.current_segment, create_segment());
            // `clear()` later walks the completed segments for their handles.
            self.completed_segments.push(segment);
        }
    }

    /// TS `pauseActivity`.
    pub async fn pause_activity(&mut self) {
        self.clear_activity_timers();
        self.stop_native_activity().await;
    }

    /// TS `resumeActivity(kind, { immediate })`.
    pub async fn resume_activity(&mut self, kind: ActivityKind, immediate: bool) {
        if self.closed {
            return;
        }
        self.clear_activity_timers();
        {
            let segment = &mut self.current_segment;
            segment.activity_kind = kind;
            segment.activity_started_at = Instant::now();
            segment.activity_armed = true;
            segment.activity_updates = 0;
        }
        if immediate {
            self.flush_activity_now(true).await;
            return;
        }
        self.schedule_activity_delay();
    }

    /// TS `clear`: drop every segment handle, then close.
    pub async fn clear(&mut self) {
        self.clear_text_timer();
        self.clear_turn_timer();
        self.clear_activity_timers();
        let mut handles: Vec<H> = self
            .completed_segments
            .iter()
            .filter_map(|segment| segment.handle.clone())
            .collect();
        handles.extend(self.current_segment.handle.clone());
        for handle in handles {
            // TS branches on which capability exists, not on the call result:
            // `if (transport.clear) … else if (transport.delete) …`.
            if let Some(clear) = self.transport.clear.clone() {
                if let Err(error) = clear(handle.clone()).await {
                    self.report_transport_error(&error, "clear");
                }
            } else if let Some(delete) = self.transport.delete.clone()
                && let Err(error) = delete(handle.clone()).await
            {
                self.report_transport_error(&error, "delete");
            }
        }
        self.stop_native_activity().await;
        self.current_segment = create_segment();
        self.completed_segments.clear();
        self.closed = true;
    }

    /// Whether the timeout terminal already ran (the wiring suppresses a later
    /// `agent_aborted` mark, WeixinChannel.ts:734-741).
    pub fn is_timed_out(&self) -> bool {
        self.timed_out
    }

    /// The weixin catch path: the visible `channel_submit_failed` agent status
    /// (WeixinChannel.ts:753-768) — a warning append plus the
    /// visible-failure latch.
    pub async fn channel_submit_failed(&mut self) {
        self.has_visible_failure_status = true;
        let text = format!("\n⚠️ {WEIXIN_SUBMIT_FAILED_TEXT}\n");
        self.append(&text).await;
    }

    // ── timer seam ─────────────────────────────────────────────────────────

    /// The earliest armed deadline, if any.
    pub fn next_deadline(&self) -> Option<Instant> {
        [
            self.text_timer,
            self.activity_delay_timer,
            self.activity_update_timer,
            self.turn_timer,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Sleep until the earliest deadline, or forever when none is armed.
    pub async fn wait_for_timer(&self) {
        match self.next_deadline() {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => std::future::pending::<()>().await,
        }
    }

    /// Fire every deadline that has come due, earliest first (the order the JS
    /// event loop would have delivered them). Firing one timer can clear or
    /// re-arm others, so each pass re-reads the deadlines.
    pub async fn fire_due_timers(&mut self) -> TimerFirings {
        let mut firings = TimerFirings::default();
        loop {
            let now = Instant::now();
            let due = [
                (self.text_timer, TimerKind::Text),
                (self.activity_delay_timer, TimerKind::ActivityDelay),
                (self.activity_update_timer, TimerKind::ActivityUpdate),
                (self.turn_timer, TimerKind::Turn),
            ]
            .into_iter()
            .filter_map(|(deadline, which)| deadline.filter(|d| *d <= now).map(|_| which))
            .min();
            match due {
                None => break,
                Some(TimerKind::Text) => {
                    self.text_timer = None;
                    self.flush_current(FlushParams {
                        final_: false,
                        force: false,
                        finalize_activity_only: false,
                    })
                    .await;
                }
                Some(TimerKind::ActivityDelay) => {
                    self.activity_delay_timer = None;
                    self.flush_activity_now(true).await;
                }
                Some(TimerKind::ActivityUpdate) => {
                    self.activity_update_timer = None;
                    self.flush_activity_now(false).await;
                }
                Some(TimerKind::Turn) => {
                    self.turn_timer = None;
                    self.mark_timed_out().await;
                    firings.turn_timed_out = true;
                }
            }
        }
        firings
    }

    // ── agent status ───────────────────────────────────────────────────────

    /// TS `handleAgentStatus` (ImLiveReplyController.ts:299-330).
    async fn handle_agent_status(&mut self, event: &JsValue) {
        let name = js_string_text(event.get("event"));
        let detail = event.get("detail").cloned().unwrap_or(JsValue::Undefined);
        // `event.detail?.visible !== false` — a missing detail counts as
        // visible; `isVisibleFailureStatusDetail` needs `severity === "error"`.
        let visible = detail.get("visible") != Some(&JsValue::Bool(false));
        let severity_error = visible && detail.get("severity") == Some(&JsValue::from("error"));
        let (visible_failure_status, warning_status) = status_event_memberships(name.as_str());
        let is_visible_failure = visible && (visible_failure_status || severity_error);
        if is_visible_failure {
            self.has_visible_failure_status = true;
        }
        let detail_message = || {
            detail
                .get("message")
                .and_then(JsValue::as_string)
                .map(|value| String::from_utf16_lossy(value.units()))
                .filter(|text| !text.is_empty())
        };
        if name == "model_empty_response_exhausted" {
            let text = detail_message().unwrap_or_else(|| EMPTY_RESPONSE_EXHAUSTED_TEXT.into());
            self.append(&format!("\n⚠️ {text}\n")).await;
            return;
        }
        if name == "max_turns_reached" {
            let text = detail_message().unwrap_or_else(|| MAX_TURNS_TEXT.into());
            self.append(&format!("\n⚠️ {text}\n")).await;
            return;
        }
        if warning_status || severity_error {
            if let Some(message) = detail_message() {
                self.append(&format!("\n⚠️ {message}\n")).await;
            }
            return;
        }
        if name == "subagent_completed" {
            self.mark_activity(ActivityKind::Thinking, None);
            return;
        }
        if name.starts_with("subagent_") {
            let detail = detail
                .get("subagentType")
                .and_then(JsValue::as_string)
                .map(|value| String::from_utf16_lossy(value.units()));
            let detail = detail.as_deref();
            self.mark_activity(ActivityKind::Subagent, detail);
        }
    }

    // ── text buffering ─────────────────────────────────────────────────────

    /// TS `append` (ImLiveReplyController.ts:332-360).
    async fn append(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.transport.edit.is_some() {
            self.clear_activity_timers();
            self.stop_native_activity().await;
        }
        let Some(limit) = self.live_text_limit() else {
            self.append_to_current_segment(text).await;
            return;
        };
        let mut rest = text.to_string();
        while !rest.is_empty() {
            let available = limit as isize - js_len(&self.current_segment.text) as isize;
            if available <= 0 {
                // Parity note (TS `append`, ImLiveReplyController.ts:343-347):
                // `flushSegment` only rotates the segment when it holds visible
                // text, so a segment made purely of whitespace never frees the
                // limit and this loop spins — **the TS controller spins the
                // same way** (probe: `_tmp/20261106-issue-1821/probe/probe3-hang.mts`
                // against the v22.22.0 baseline exits 124 with no output).
                // Kept byte-for-byte faithful; tracked as a P2 follow-up rather
                // than "fixed" into a divergence from the baseline.
                self.flush_segment(true).await;
                continue;
            }
            let piece = js_prefix(&rest, available as usize).to_string();
            self.append_to_current_segment(&piece).await;
            rest = js_suffix(&rest, js_len(&piece)).to_string();
            if !rest.is_empty() {
                self.flush_segment(true).await;
            }
        }
    }

    /// TS `appendToCurrentSegment` (ImLiveReplyController.ts:362-388).
    async fn append_to_current_segment(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        {
            let segment = &mut self.current_segment;
            segment.text.push_str(text);
            if segment.first_text_at.is_none() {
                segment.first_text_at = Some(Instant::now());
            }
        }
        if !self.can_send_non_final_text() {
            return;
        }
        if !self.has_visible_reply_text() {
            if js_len(&self.current_segment.text) >= self.options.initial_buffer_threshold {
                self.flush_current(FlushParams {
                    final_: false,
                    force: true,
                    finalize_activity_only: false,
                })
                .await;
                return;
            }
            self.schedule_initial_text_flush();
            return;
        }
        let pending = js_len(&self.current_segment.text)
            .saturating_sub(js_len(&self.current_segment.last_visible_final_text));
        if pending >= self.options.buffer_threshold {
            self.flush_current(FlushParams {
                final_: false,
                force: true,
                finalize_activity_only: false,
            })
            .await;
            return;
        }
        self.schedule_text_flush(self.options.throttle_ms);
    }

    /// TS `canSendNonFinalText`.
    fn can_send_non_final_text(&self) -> bool {
        self.transport.edit.is_some() && !self.current_segment.edit_disabled
    }

    /// TS `hasVisibleReplyText`.
    fn has_visible_reply_text(&self) -> bool {
        !self.current_segment.last_visible_final_text.is_empty()
    }

    /// TS `liveTextLimit`: `max(1, maxMessageLength - cursor.length)`, only
    /// when the transport can edit.
    fn live_text_limit(&self) -> Option<usize> {
        self.transport.edit.as_ref()?;
        let max = self.transport.max_message_length?;
        // TS `if (!max) return undefined;` — a zero max means "no live text
        // limit", not a limit of one.
        if max == 0 {
            return None;
        }
        Some(max.saturating_sub(js_len(&self.options.cursor)).max(1))
    }

    /// TS `scheduleInitialTextFlush`.
    fn schedule_initial_text_flush(&mut self) {
        let started_at = self
            .current_segment
            .first_text_at
            .unwrap_or_else(Instant::now);
        let elapsed = (Instant::now() - started_at).as_millis() as u64;
        let delay = self.options.initial_throttle_ms.saturating_sub(elapsed);
        self.schedule_text_flush(delay);
    }

    /// TS `scheduleTextFlush` — the deadline instead of the callback.
    fn schedule_text_flush(&mut self, delay_ms: u64) {
        if self.text_timer.is_some() || self.closed {
            return;
        }
        self.text_timer = Some(Instant::now() + Duration::from_millis(delay_ms));
    }

    /// TS `armTurnTimer` (ImLiveReplyController.ts:420-430).
    fn arm_turn_timer(&mut self, force: bool) {
        if self.closed || self.options.turn_timeout_ms == 0 {
            return;
        }
        if self.turn_timer_armed && !force {
            return;
        }
        self.clear_turn_timer();
        self.turn_timer_armed = true;
        self.turn_timer =
            Some(Instant::now() + Duration::from_millis(self.options.turn_timeout_ms));
    }

    // ── activity ───────────────────────────────────────────────────────────

    /// TS `markActivity` (ImLiveReplyController.ts:432-454).
    fn mark_activity(&mut self, kind: ActivityKind, detail: Option<&str>) {
        if !self.can_use_activity() {
            return;
        }
        if self.current_segment.activity_disabled || self.should_suppress_activity_for_text() {
            return;
        }
        let kind_changed = self.current_segment.activity_kind != kind;
        if kind_changed || !self.current_segment.activity_armed {
            self.clear_activity_timers();
            let segment = &mut self.current_segment;
            segment.activity_kind = kind;
            segment.activity_started_at = Instant::now();
            segment.activity_armed = true;
            segment.activity_updates = 0;
        }
        if let Some(detail) = detail {
            let trimmed = crate::metadata::js_trim(detail);
            if !trimmed.is_empty() {
                self.current_segment.activity_detail = Some(js_prefix(trimmed, 160).to_string());
            }
        }
        if self.current_segment.activity_visible && !kind_changed {
            self.schedule_activity_update();
            return;
        }
        self.schedule_activity_delay();
    }

    /// TS `canUseActivity`.
    fn can_use_activity(&self) -> bool {
        self.transport.pulse_activity.is_some() || self.transport.edit.is_some()
    }

    /// TS `scheduleActivityDelay` (ImLiveReplyController.ts:460-471).
    fn schedule_activity_delay(&mut self) {
        if self.closed || self.activity_delay_timer.is_some() || !self.can_use_activity() {
            return;
        }
        if self.current_segment.activity_disabled
            || self.should_suppress_activity_for_text()
            || self.options.activity_delay_ms < 0
        {
            return;
        }
        self.activity_delay_timer =
            Some(Instant::now() + Duration::from_millis(self.options.activity_delay_ms as u64));
    }

    /// TS `scheduleActivityUpdate` (ImLiveReplyController.ts:473-493).
    fn schedule_activity_update(&mut self) {
        if self.closed || self.activity_update_timer.is_some() || !self.can_use_activity() {
            return;
        }
        if self.current_segment.activity_disabled
            || !self.current_segment.activity_visible
            || self.should_suppress_activity_for_text()
            || self.current_segment.activity_updates >= self.options.activity_max_updates
        {
            return;
        }
        let elapsed =
            (Instant::now() - self.current_segment.activity_started_at).as_millis() as u64;
        if elapsed >= self.options.activity_ttl_ms {
            return;
        }
        self.activity_update_timer =
            Some(Instant::now() + Duration::from_millis(self.options.activity_update_throttle_ms));
    }

    /// TS `flushActivityNow` (ImLiveReplyController.ts:499-591).
    async fn flush_activity_now(&mut self, force: bool) {
        if self.closed
            || !self.can_use_activity()
            || self.current_segment.activity_disabled
            || self.should_suppress_activity_for_text()
            || self.current_segment.activity_updates >= self.options.activity_max_updates
        {
            return;
        }
        let elapsed =
            (Instant::now() - self.current_segment.activity_started_at).as_millis() as u64;
        if elapsed >= self.options.activity_ttl_ms {
            return;
        }
        let mut activity = LiveReplyActivity {
            kind: self.current_segment.activity_kind,
            text: String::new(),
            elapsed_ms: elapsed,
            update_count: self.current_segment.activity_updates + 1,
            detail: self.current_segment.activity_detail.clone(),
        };
        activity.text = (self.options.format_activity)(&activity);
        if let Some(pulse) = self.transport.pulse_activity.clone() {
            match pulse(activity).await {
                Ok(true) => {
                    self.current_segment.activity_visible = true;
                    self.current_segment.activity_updates += 1;
                    self.schedule_activity_update();
                }
                Ok(false) => {
                    self.current_segment.activity_disabled = true;
                }
                Err(error) => {
                    self.report_transport_error(&error, "activity");
                    self.current_segment.activity_disabled = true;
                }
            }
            return;
        }
        if self.transport.edit.is_none() {
            return;
        }
        let formatted =
            self.format_for_transport(&format!("{}{}", activity.text, self.options.cursor));
        if !force && formatted == self.current_segment.last_visible_text {
            self.current_segment.activity_updates += 1;
            self.schedule_activity_update();
            return;
        }
        if let Some(handle) = self.current_segment.handle.clone() {
            let edit = self.transport.edit.clone().expect("#1821 activity edit");
            match edit(handle, formatted.clone()).await {
                Ok(true) => {
                    let segment = &mut self.current_segment;
                    segment.last_visible_text = formatted;
                    segment.activity_visible = true;
                    segment.activity_updates += 1;
                }
                Ok(false) => {
                    let segment = &mut self.current_segment;
                    segment.activity_disabled = true;
                    segment.edit_disabled = true;
                    segment.fallback_prefix = String::new();
                }
                Err(error) => {
                    self.report_transport_error(&error, "edit");
                    let segment = &mut self.current_segment;
                    segment.activity_disabled = true;
                    segment.edit_disabled = true;
                    segment.fallback_prefix = String::new();
                }
            }
            self.schedule_activity_update();
            return;
        }
        let send = self.transport.send.clone();
        match send(formatted.clone()).await {
            Ok(SendOutcome::Handle(handle)) => {
                let segment = &mut self.current_segment;
                segment.handle = Some(handle);
                segment.last_visible_text = formatted;
                segment.activity_visible = true;
                segment.activity_updates += 1;
                self.schedule_activity_update();
            }
            Ok(SendOutcome::NoHandle) => {
                self.current_segment.activity_disabled = true;
                self.current_segment.edit_disabled = true;
            }
            Ok(SendOutcome::Failed) => {
                self.current_segment.activity_disabled = true;
            }
            Err(error) => {
                self.report_transport_error(&error, "send");
                self.current_segment.activity_disabled = true;
            }
        }
    }

    // ── flushing ───────────────────────────────────────────────────────────

    /// TS `flushCurrent` (clear the text timer, then the flush body).
    async fn flush_current(&mut self, params: FlushParams) {
        self.clear_text_timer();
        self.flush_current_now(params).await;
    }

    /// TS `flushCurrentNow` (ImLiveReplyController.ts:629-733).
    async fn flush_current_now(&mut self, params: FlushParams) {
        let final_text = self.current_segment.text.clone();
        let has_text = !crate::metadata::js_trim(&final_text).is_empty();
        if !has_text {
            if params.final_
                && params.finalize_activity_only
                && self.current_segment.activity_visible
            {
                self.finalize_activity_only().await;
            }
            return;
        }
        if !params.final_ && !self.can_send_non_final_text() {
            return;
        }
        if params.final_ && self.transport.edit.is_none() && self.current_segment.handle.is_none() {
            self.send_final_chunks(&final_text).await;
            return;
        }
        if self.current_segment.edit_disabled {
            if params.final_ {
                self.send_fallback_continuation(&final_text).await;
            }
            return;
        }
        let visible_text = if params.final_ {
            final_text.clone()
        } else {
            format!("{}{}", final_text, self.options.cursor)
        };
        let formatted = self.format_for_transport(&visible_text);
        if !params.force && formatted == self.current_segment.last_visible_text {
            return;
        }
        if self.current_segment.handle.is_some() && self.transport.edit.is_some() {
            let handle = self
                .current_segment
                .handle
                .clone()
                .expect("#1821 text edit handle");
            let edit = self
                .transport
                .edit
                .clone()
                .expect("#1821 text edit transport");
            match edit(handle, formatted.clone()).await {
                Ok(true) => {
                    let segment = &mut self.current_segment;
                    segment.activity_visible = false;
                    segment.last_visible_text = formatted;
                    segment.last_visible_final_text = final_text;
                }
                Ok(false) => {
                    self.enter_fallback_mode().await;
                    if params.final_ {
                        self.send_fallback_continuation(&final_text).await;
                    }
                }
                Err(error) => {
                    self.report_transport_error(&error, "edit");
                    self.enter_fallback_mode().await;
                    if params.final_ {
                        self.send_fallback_continuation(&final_text).await;
                    }
                }
            }
            return;
        }
        if self.current_segment.handle.is_some() {
            // `handle && !edit`: the transport cannot edit what it already
            // sent, so the remainder continues as fallback.
            self.current_segment.edit_disabled = true;
            self.current_segment.fallback_prefix =
                self.current_segment.last_visible_final_text.clone();
            if params.final_ {
                self.send_fallback_continuation(&final_text).await;
            }
            return;
        }
        let send = self.transport.send.clone();
        match send(formatted.clone()).await {
            Ok(SendOutcome::Failed) => {
                self.current_segment.edit_disabled = true;
                self.current_segment.fallback_prefix = String::new();
                if params.final_ {
                    self.send_fallback_continuation(&final_text).await;
                }
            }
            Ok(outcome) => {
                let handle = match &outcome {
                    SendOutcome::Handle(handle) => Some(handle.clone()),
                    _ => None,
                };
                let segment = &mut self.current_segment;
                segment.handle = handle;
                segment.activity_visible = false;
                segment.last_visible_text = formatted;
                segment.last_visible_final_text = final_text.clone();
                if matches!(outcome, SendOutcome::NoHandle) || self.transport.edit.is_none() {
                    segment.edit_disabled = true;
                    segment.fallback_prefix = final_text;
                }
            }
            Err(error) => {
                self.report_transport_error(&error, "send");
                self.current_segment.edit_disabled = true;
                self.current_segment.fallback_prefix = String::new();
                if params.final_ {
                    self.send_fallback_continuation(&final_text).await;
                }
            }
        }
    }

    /// TS `finalizeActivityOnly` (ImLiveReplyController.ts:735-771) over the
    /// current segment (its only TS caller passes `this.currentSegment`).
    async fn finalize_activity_only(&mut self) {
        self.stop_native_activity().await;
        let formatted = self.format_for_transport(&self.options.activity_only_final_text.clone());
        let has_handle = self.current_segment.handle.is_some();
        if !has_handle || self.transport.edit.is_none() || self.current_segment.edit_disabled {
            if self.transport.edit.is_none() && self.current_segment.activity_visible {
                let send = self.transport.send.clone();
                match send(formatted.clone()).await {
                    Ok(SendOutcome::Failed) => {}
                    Ok(outcome) => {
                        if let SendOutcome::Handle(handle) = outcome {
                            self.current_segment.handle = Some(handle);
                        }
                        let segment = &mut self.current_segment;
                        segment.last_visible_text = formatted.clone();
                        segment.last_visible_final_text = formatted;
                    }
                    Err(error) => self.report_transport_error(&error, "send"),
                }
            }
            return;
        }
        let handle = self
            .current_segment
            .handle
            .clone()
            .expect("#1821 activity-only edit handle");
        let edit = self
            .transport
            .edit
            .clone()
            .expect("#1821 activity-only edit transport");
        match edit(handle, formatted.clone()).await {
            Ok(true) => {
                let segment = &mut self.current_segment;
                segment.activity_visible = false;
                segment.last_visible_text = formatted.clone();
                segment.last_visible_final_text = formatted;
            }
            Ok(false) => {
                self.current_segment.edit_disabled = true;
            }
            Err(error) => {
                self.report_transport_error(&error, "edit");
                self.current_segment.edit_disabled = true;
            }
        }
    }

    /// TS `finalizeWithTerminalText` (ImLiveReplyController.ts:598-607).
    async fn finalize_with_terminal_text(&mut self, text: &str) {
        if self.closed {
            return;
        }
        self.clear_text_timer();
        self.clear_turn_timer();
        self.clear_activity_timers();
        self.stop_native_activity().await;
        self.finalize_with_terminal_text_now(text).await;
        self.closed = true;
    }

    /// TS `finalizeWithTerminalTextNow` (ImLiveReplyController.ts:773-808).
    async fn finalize_with_terminal_text_now(&mut self, text: &str) {
        let formatted = self.format_for_transport(text);
        {
            let segment = &mut self.current_segment;
            segment.text = String::new();
            segment.activity_visible = false;
            segment.last_visible_final_text = formatted.clone();
        }
        let mut edit_landed = false;
        if self.current_segment.handle.is_some()
            && self.transport.edit.is_some()
            && !self.current_segment.edit_disabled
        {
            let handle = self
                .current_segment
                .handle
                .clone()
                .expect("#1821 terminal edit handle");
            let edit = self
                .transport
                .edit
                .clone()
                .expect("#1821 terminal edit transport");
            match edit(handle, formatted.clone()).await {
                Ok(true) => {
                    self.current_segment.last_visible_text = formatted.clone();
                    edit_landed = true;
                }
                Ok(false) => {}
                Err(error) => self.report_transport_error(&error, "edit"),
            }
            if !edit_landed {
                self.current_segment.edit_disabled = true;
            }
        }
        if edit_landed {
            return;
        }
        let send_needed = self.current_segment.handle.is_none()
            || crate::metadata::js_trim(&self.current_segment.last_visible_text).is_empty()
            || self.transport.edit.is_some();
        if !send_needed {
            return;
        }
        let send = self.transport.send.clone();
        match send(formatted.clone()).await {
            Ok(SendOutcome::Handle(handle)) => {
                self.current_segment.handle = Some(handle);
                self.current_segment.last_visible_text = formatted;
            }
            Ok(SendOutcome::NoHandle) => {
                self.current_segment.last_visible_text = formatted;
            }
            Ok(SendOutcome::Failed) => {}
            Err(error) => self.report_transport_error(&error, "send"),
        }
    }

    /// TS `enterFallbackMode`.
    async fn enter_fallback_mode(&mut self) {
        if self.current_segment.edit_disabled {
            return;
        }
        self.current_segment.edit_disabled = true;
        self.current_segment.fallback_prefix = self.current_segment.last_visible_final_text.clone();
        self.strip_cursor_best_effort().await;
    }

    /// TS `stripCursorBestEffort`.
    async fn strip_cursor_best_effort(&mut self) {
        if self.current_segment.handle.is_none()
            || self.transport.edit.is_none()
            || self.options.cursor.is_empty()
        {
            return;
        }
        if !self
            .current_segment
            .last_visible_text
            .ends_with(&self.options.cursor)
        {
            return;
        }
        let clean = js_prefix(
            &self.current_segment.last_visible_text,
            js_len(&self.current_segment.last_visible_text) - js_len(&self.options.cursor),
        )
        .to_string();
        let handle = self
            .current_segment
            .handle
            .clone()
            .expect("#1821 strip-cursor edit handle");
        let edit = self
            .transport
            .edit
            .clone()
            .expect("#1821 strip-cursor edit transport");
        match edit(handle, clean.clone()).await {
            Ok(true) => self.current_segment.last_visible_text = clean,
            Ok(false) => {}
            Err(error) => self.report_transport_error(&error, "edit"),
        }
    }

    /// TS `sendFallbackContinuation` (ImLiveReplyController.ts:832-859).
    async fn send_fallback_continuation(&mut self, final_text: &str) {
        let continuation = continuation_text(final_text, &self.current_segment.fallback_prefix);
        if crate::metadata::js_trim(&continuation).is_empty() {
            self.current_segment.last_visible_final_text = final_text.to_string();
            return;
        }
        for chunk in self.split_for_transport(&continuation) {
            let send = self.transport.send.clone();
            match send(chunk.clone()).await {
                Ok(SendOutcome::Failed) => return,
                Ok(outcome) => {
                    let handle = match &outcome {
                        SendOutcome::Handle(handle) => Some(handle.clone()),
                        _ => None,
                    };
                    let segment = &mut self.current_segment;
                    segment.handle = handle;
                    segment.activity_visible = false;
                    segment.last_visible_text = chunk;
                    segment.last_visible_final_text = final_text.to_string();
                    if matches!(outcome, SendOutcome::Handle(_)) && self.transport.edit.is_some() {
                        segment.edit_disabled = false;
                        segment.fallback_prefix = final_text.to_string();
                    }
                }
                Err(error) => {
                    self.report_transport_error(&error, "send");
                    return;
                }
            }
        }
        self.current_segment.fallback_prefix = final_text.to_string();
    }

    /// TS `shouldFlushBeforeToolBoundary`.
    fn should_flush_before_tool_boundary(&self) -> bool {
        self.transport.edit.is_some()
            || self.current_segment.handle.is_some()
            || self.has_visible_reply_text()
    }

    /// TS `shouldSuppressActivityForText`.
    fn should_suppress_activity_for_text(&self) -> bool {
        if self.transport.edit.is_none() && self.transport.pulse_activity.is_some() {
            return false;
        }
        !crate::metadata::js_trim(&self.current_segment.text).is_empty()
            || self.has_visible_reply_text()
    }

    /// TS `sendFinalChunks` (ImLiveReplyController.ts:873-890).
    async fn send_final_chunks(&mut self, final_text: &str) {
        for chunk in self.split_for_transport(final_text) {
            let send = self.transport.send.clone();
            match send(chunk.clone()).await {
                Ok(SendOutcome::Failed) => return,
                Ok(outcome) => {
                    if let SendOutcome::Handle(handle) = outcome {
                        self.current_segment.handle = Some(handle);
                    }
                    let segment = &mut self.current_segment;
                    segment.activity_visible = false;
                    segment.last_visible_text = chunk;
                    segment.last_visible_final_text = final_text.to_string();
                }
                Err(error) => {
                    self.report_transport_error(&error, "send");
                    return;
                }
            }
        }
    }

    /// TS `formatForTransport`: format, then truncate to the transport max.
    fn format_for_transport(&self, text: &str) -> String {
        let formatted = match &self.transport.format_text {
            Some(format) => format(text),
            None => text.to_string(),
        };
        match self.transport.max_message_length {
            // TS `if (!max …)`: `0` is falsy, so it means "no limit".
            Some(max) if max > 0 && js_len(&formatted) > max => {
                js_prefix(&formatted, max).to_string()
            }
            _ => formatted,
        }
    }

    /// TS `splitForTransport` (ImLiveReplyController.ts:901-919): format once
    /// more (TS formats twice — deliberately kept).
    fn split_for_transport(&self, text: &str) -> Vec<String> {
        let formatted = match &self.transport.format_text {
            Some(format) => format(text),
            None => text.to_string(),
        };
        let Some(max) = self.transport.max_message_length else {
            return vec![formatted];
        };
        // TS `if (!max …)`: `0` is falsy, so a zero max disables splitting
        // (without this the loop below never shortens `rest`).
        if max == 0 || js_len(&formatted) <= max {
            return vec![formatted];
        }
        let mut chunks = Vec::new();
        let mut rest = formatted;
        while js_len(&rest) > max {
            let mut split = last_index_of(&rest, '\n', max);
            if 2 * split < max as isize {
                split = last_index_of(&rest, ' ', max);
            }
            if 2 * split < max as isize {
                split = max as isize;
            }
            chunks.push(js_prefix(&rest, split as usize).to_string());
            rest = js_suffix(&rest, split as usize)
                .trim_start_matches('\n')
                .to_string();
        }
        if !rest.is_empty() {
            chunks.push(rest);
        }
        chunks
    }

    // ── timers / transport helpers ─────────────────────────────────────────

    fn clear_text_timer(&mut self) {
        self.text_timer = None;
    }

    fn clear_turn_timer(&mut self) {
        if self.turn_timer.is_some() {
            self.turn_timer = None;
        }
        self.turn_timer_armed = false;
    }

    fn clear_activity_timers(&mut self) {
        self.activity_delay_timer = None;
        self.activity_update_timer = None;
    }

    /// TS `stopNativeActivity`.
    async fn stop_native_activity(&mut self) {
        let Some(stop) = self.transport.stop_activity.clone() else {
            return;
        };
        if let Err(error) = stop().await {
            self.report_transport_error(&error, "stopActivity");
        }
    }

    /// TS `reportTransportError`: `(error, phase)`.
    fn report_transport_error(&self, error: &str, phase: &str) {
        if let Some(report) = &self.options.on_transport_error {
            report(error, phase);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TimerKind {
    Text,
    ActivityDelay,
    ActivityUpdate,
    Turn,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The §3 goldens use two probe label styles for `pulseActivity`; the mock
    /// records whichever the test asserts against (the call sequence, not the
    /// label spelling, is the pinned contract).
    #[derive(Clone, Copy)]
    enum PulseLabel {
        Detailed,
        Plain,
    }

    struct Mock {
        calls: Vec<String>,
        next_handle: u64,
        send_calls: u64,
    }
    type Shared = Arc<Mutex<Mock>>;

    fn shared_mock() -> Shared {
        Arc::new(Mutex::new(Mock {
            calls: Vec::new(),
            next_handle: 1,
            send_calls: 0,
        }))
    }

    fn recorded(shared: &Shared) -> Vec<String> {
        shared.lock().expect("#1821 mock lock").calls.clone()
    }

    #[derive(Default)]
    struct Caps {
        edit: bool,
        stop: bool,
        delete: bool,
        clear: bool,
        pulse: Option<PulseLabel>,
        max_message_length: Option<usize>,
        send_no_handle: bool,
        send_fail_once: bool,
        format_text: Option<TransportFormatText>,
    }

    fn event(kind: &'static str) -> JsValue {
        crate::metadata::object([("type", kind.into())])
    }

    fn text_event(kind: &'static str, text: &str) -> JsValue {
        crate::metadata::object([("type", kind.into()), ("text", text.into())])
    }

    /// A recording transport: `send`/`edit`/`pulse`/`stopActivity` append one
    /// label per call, futures resolve immediately, and `send` hands out
    /// sequential handles (or `NoHandle` for the weixin shape).
    fn controller(
        shared: &Shared,
        caps: Caps,
        options: LiveReplyOptions,
    ) -> LiveReplyController<u64> {
        let send = {
            let shared = shared.clone();
            let no_handle = caps.send_no_handle;
            let fail_once = caps.send_fail_once;
            Arc::new(move |text: String| {
                let mut mock = shared.lock().expect("#1821 mock lock");
                mock.calls.push(format!("send:\"{text}\""));
                mock.send_calls += 1;
                let outcome = if no_handle {
                    SendOutcome::NoHandle
                } else if fail_once && mock.send_calls == 1 {
                    SendOutcome::Failed
                } else {
                    let handle = mock.next_handle;
                    mock.next_handle += 1;
                    SendOutcome::Handle(handle)
                };
                Box::pin(std::future::ready(Ok(outcome))) as TransportFuture<SendOutcome<u64>>
            }) as Arc<dyn Fn(String) -> TransportFuture<SendOutcome<u64>> + Send + Sync>
        };
        let edit = caps.edit.then(|| {
            let shared = shared.clone();
            Arc::new(move |handle: u64, text: String| {
                let mut mock = shared.lock().expect("#1821 mock lock");
                mock.calls.push(format!("edit:{handle}:\"{text}\""));
                Box::pin(std::future::ready(Ok(true))) as TransportFuture<bool>
            }) as Arc<dyn Fn(u64, String) -> TransportFuture<bool> + Send + Sync>
        });
        let pulse_activity = caps.pulse.map(|label| {
            let shared = shared.clone();
            Arc::new(move |activity: LiveReplyActivity| {
                let mut mock = shared.lock().expect("#1821 mock lock");
                let record = match label {
                    PulseLabel::Detailed => format!(
                        "pulse:{}:{}:{}:\"{}\"",
                        activity.kind.as_str(),
                        activity.elapsed_ms,
                        activity.update_count,
                        activity.text
                    ),
                    PulseLabel::Plain => format!("pulse:\"{}\"", activity.text),
                };
                mock.calls.push(record);
                Box::pin(std::future::ready(Ok(true))) as TransportFuture<bool>
            }) as Arc<dyn Fn(LiveReplyActivity) -> TransportFuture<bool> + Send + Sync>
        });
        let delete = caps.delete.then(|| {
            let shared = shared.clone();
            Arc::new(move |handle: u64| {
                let mut mock = shared.lock().expect("#1821 mock lock");
                mock.calls.push(format!("delete:{handle}"));
                Box::pin(std::future::ready(Ok(true))) as TransportFuture<bool>
            }) as Arc<dyn Fn(u64) -> TransportFuture<bool> + Send + Sync>
        });
        let clear = caps.clear.then(|| {
            let shared = shared.clone();
            Arc::new(move |handle: u64| {
                let mut mock = shared.lock().expect("#1821 mock lock");
                mock.calls.push(format!("clear:{handle}"));
                Box::pin(std::future::ready(Ok(true))) as TransportFuture<bool>
            }) as Arc<dyn Fn(u64) -> TransportFuture<bool> + Send + Sync>
        });
        let stop_activity = caps.stop.then(|| {
            let shared = shared.clone();
            Arc::new(move || {
                let mut mock = shared.lock().expect("#1821 mock lock");
                mock.calls.push("stopActivity".into());
                Box::pin(std::future::ready(Ok(true))) as TransportFuture<bool>
            }) as Arc<dyn Fn() -> TransportFuture<bool> + Send + Sync>
        });
        LiveReplyController::new(
            LiveReplyTransport {
                send,
                edit,
                delete,
                clear,
                pulse_activity,
                stop_activity,
                max_message_length: caps.max_message_length,
                format_text: caps.format_text,
            },
            options,
        )
    }

    async fn advance(millis: u64) {
        tokio::time::advance(Duration::from_millis(millis)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn edit_intermediate_updates_follow_the_ts_request_sequence() {
        let shared = shared_mock();
        let options = LiveReplyOptions {
            throttle_ms: 5,
            initial_throttle_ms: 5,
            buffer_threshold: 8,
            initial_buffer_threshold: 4,
            ..LiveReplyOptions::default()
        };
        let mut reply = controller(
            &shared,
            Caps {
                edit: true,
                stop: true,
                ..Caps::default()
            },
            options,
        );
        reply.handle_event(&event("turn_started")).await;
        for delta in ["ab", "cd", "efghijklmn", "op"] {
            reply
                .handle_event(&text_event("assistant_text_delta", delta))
                .await;
            advance(6).await;
            reply.fire_due_timers().await;
        }
        reply.flush_final().await;
        crate::negative_assert_eq!(
            recorded(&shared),
            vec![
                "stopActivity".to_string(),
                "send:\"ab ▉\"".to_string(),
                "stopActivity".to_string(),
                "edit:1:\"abcd ▉\"".to_string(),
                "stopActivity".to_string(),
                "edit:1:\"abcdefghijklmn ▉\"".to_string(),
                "stopActivity".to_string(),
                "edit:1:\"abcdefghijklmnop ▉\"".to_string(),
                "edit:1:\"abcdefghijklmnop\"".to_string(),
                "stopActivity".to_string(),
            ],
            "#1821 edit intermediate updates follow the ts request sequence"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn live_text_limit_splits_before_max_message_length() {
        let shared = shared_mock();
        let options = LiveReplyOptions {
            throttle_ms: 5,
            initial_throttle_ms: 5,
            buffer_threshold: 6,
            initial_buffer_threshold: 3,
            ..LiveReplyOptions::default()
        };
        let mut reply = controller(
            &shared,
            Caps {
                edit: true,
                stop: true,
                max_message_length: Some(12),
                ..Caps::default()
            },
            options,
        );
        reply.handle_event(&event("turn_started")).await;
        reply
            .handle_event(&text_event(
                "assistant_text_delta",
                "alpha beta\ngamma delta epsilon",
            ))
            .await;
        reply.flush_final().await;
        crate::negative_assert_eq!(
            recorded(&shared),
            vec![
                "stopActivity".to_string(),
                "send:\"alpha beta ▉\"".to_string(),
                "edit:1:\"alpha beta\"".to_string(),
                "send:\"\ngamma del ▉\"".to_string(),
                "edit:2:\"\ngamma del\"".to_string(),
                "send:\"ta epsilon ▉\"".to_string(),
                "edit:3:\"ta epsilon\"".to_string(),
                "stopActivity".to_string(),
            ],
            "#1821 live text limit splits before max message length"
        );

        let split = shared_mock();
        let mut reply = controller(
            &split,
            Caps {
                max_message_length: Some(20),
                ..Caps::default()
            },
            LiveReplyOptions::default(),
        );
        reply.handle_event(&event("turn_started")).await;
        reply
            .handle_event(&text_event(
                "assistant_text_delta",
                "aaa bbb ccc ddd eee fff ggg hhh",
            ))
            .await;
        reply.flush_final().await;
        crate::negative_assert_eq!(
            recorded(&split),
            vec![
                "send:\"aaa bbb ccc ddd eee\"".to_string(),
                "send:\" fff ggg hhh\"".to_string(),
            ],
            "#1821 no-edit transports split the final text at the max message length"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn format_text_then_truncate_then_split() {
        let shared = shared_mock();
        let mut reply = controller(
            &shared,
            Caps {
                max_message_length: Some(10),
                format_text: Some(Arc::new(|text: &str| format!("[{text}]"))),
                ..Caps::default()
            },
            LiveReplyOptions::default(),
        );
        reply.handle_event(&event("turn_started")).await;
        reply
            .handle_event(&text_event("assistant_text_delta", "0123456789abcdef"))
            .await;
        reply.flush_final().await;
        crate::negative_assert_eq!(
            recorded(&shared),
            vec![
                "send:\"[012345678\"".to_string(),
                "send:\"9abcdef]\"".to_string(),
            ],
            "#1821 formatText runs before the max message length truncate and split"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn activity_pulses_fire_after_the_delay_and_respect_max_updates() {
        let shared = shared_mock();
        let options = LiveReplyOptions {
            activity_delay_ms: 10,
            activity_update_throttle_ms: 10,
            activity_max_updates: 2,
            activity_ttl_ms: 10_000,
            format_activity: Arc::new(|activity: &LiveReplyActivity| {
                match activity.detail.as_deref() {
                    Some(detail) => {
                        format!(
                            "A{}@{}:{detail}",
                            activity.kind.as_str(),
                            activity.elapsed_ms
                        )
                    }
                    None => format!("A{}@{}", activity.kind.as_str(), activity.elapsed_ms),
                }
            }),
            ..LiveReplyOptions::default()
        };
        let mut reply = controller(
            &shared,
            Caps {
                pulse: Some(PulseLabel::Detailed),
                stop: true,
                ..Caps::default()
            },
            options,
        );
        reply.handle_event(&event("turn_started")).await;
        advance(12).await;
        reply.fire_due_timers().await;
        reply.handle_event(&event("model_request_started")).await;
        advance(11).await;
        reply.fire_due_timers().await;
        // activityUpdates hit the cap: a third mark schedules nothing.
        reply.handle_event(&event("model_request_started")).await;
        advance(50).await;
        reply.fire_due_timers().await;
        reply.pause_activity().await;
        reply.pause_activity().await;
        crate::negative_assert_eq!(
            recorded(&shared),
            vec![
                "pulse:thinking:12:1:\"Athinking@12\"".to_string(),
                "pulse:thinking:23:2:\"Athinking@23\"".to_string(),
                "stopActivity".to_string(),
                "stopActivity".to_string(),
            ],
            "#1821 activity pulses must fire activity-pulses-fire-after-the-delay-and-respect-max-updates"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn activity_delay_is_silent_just_before_ten_minutes() {
        let shared = shared_mock();
        let options = LiveReplyOptions {
            activity_delay_ms: 600_000,
            activity_update_throttle_ms: 600_000,
            activity_max_updates: u32::MAX,
            activity_ttl_ms: u64::MAX,
            turn_timeout_ms: 0,
            format_activity: Arc::new(format_weixin_activity),
            ..LiveReplyOptions::default()
        };
        let mut reply = controller(
            &shared,
            Caps {
                pulse: Some(PulseLabel::Plain),
                ..Caps::default()
            },
            options,
        );
        reply.handle_event(&event("turn_started")).await;
        advance(599_999).await;
        reply.fire_due_timers().await;
        crate::negative_assert!(
            recorded(&shared).is_empty(),
            "#1821 activity pulses must fire activity-delay-is-silent-just-before-ten-minutes"
        );
        advance(1).await;
        reply.fire_due_timers().await;
        crate::negative_assert_eq!(
            recorded(&shared),
            vec!["pulse:\"仍在处理：正在分析和生成回复（已用时 10 分钟）\"".to_string()],
            "#1821 activity pulses must fire activity-delay-is-silent-just-before-ten-minutes"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn turn_timeout_marks_timed_out_and_reports_the_timer() {
        let shared = shared_mock();
        let options = LiveReplyOptions {
            turn_timeout_ms: 10,
            timeout_final_text: "TO".into(),
            ..LiveReplyOptions::default()
        };
        let mut reply = controller(
            &shared,
            Caps {
                stop: true,
                ..Caps::default()
            },
            options,
        );
        reply.handle_event(&event("turn_started")).await;
        advance(10).await;
        let fired = reply.fire_due_timers().await;
        crate::negative_assert_eq!(
            fired,
            TimerFirings {
                turn_timed_out: true
            },
            "#1821 turn timeout watchdog fires turn-timeout-marks-timed-out-and-reports-the-timer"
        );
        crate::negative_assert!(
            reply.is_timed_out(),
            "#1821 turn timeout watchdog fires turn-timeout-marks-timed-out-and-reports-the-timer"
        );
        crate::negative_assert_eq!(
            reply.next_deadline(),
            None,
            "#1821 turn timeout watchdog fires turn-timeout-marks-timed-out-and-reports-the-timer"
        );
        crate::negative_assert_eq!(
            recorded(&shared),
            vec!["stopActivity".to_string(), "send:\"TO\"".to_string()],
            "#1821 turn timeout watchdog fires turn-timeout-marks-timed-out-and-reports-the-timer"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn zero_turn_timeout_never_arms_the_watchdog() {
        let shared = shared_mock();
        let options = LiveReplyOptions {
            turn_timeout_ms: 0,
            ..LiveReplyOptions::default()
        };
        let mut reply = controller(&shared, Caps::default(), options);
        reply.handle_event(&event("turn_started")).await;
        advance(60_000).await;
        let fired = reply.fire_due_timers().await;
        crate::negative_assert_eq!(
            fired,
            TimerFirings::default(),
            "#1821 zero turn timeout never arms the watchdog"
        );
        crate::negative_assert!(
            !reply.is_timed_out(),
            "#1821 zero turn timeout never arms the watchdog"
        );
        crate::negative_assert_eq!(
            recorded(&shared),
            Vec::<String>::new(),
            "#1821 zero turn timeout never arms the watchdog"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn pause_and_resume_activity_follow_the_ts_sequence() {
        let shared = shared_mock();
        let options = LiveReplyOptions {
            activity_delay_ms: 10,
            activity_update_throttle_ms: 10,
            activity_max_updates: 2,
            activity_ttl_ms: 10_000,
            ..LiveReplyOptions::default()
        };
        let mut reply = controller(
            &shared,
            Caps {
                pulse: Some(PulseLabel::Plain),
                stop: true,
                ..Caps::default()
            },
            options,
        );
        reply.resume_activity(ActivityKind::Thinking, true).await;
        reply.pause_activity().await;
        reply.resume_activity(ActivityKind::Thinking, false).await;
        advance(10).await;
        reply.fire_due_timers().await;
        reply.resume_activity(ActivityKind::Thinking, false).await;
        advance(10).await;
        reply.fire_due_timers().await;
        reply.flush_final().await;
        crate::negative_assert_eq!(
            recorded(&shared),
            vec![
                "pulse:\"正在思考…\"".to_string(),
                "stopActivity".to_string(),
                "pulse:\"正在思考…\"".to_string(),
                "pulse:\"正在思考…\"".to_string(),
                "stopActivity".to_string(),
                "send:\"处理完成，但没有可见回复。\"".to_string(),
                "stopActivity".to_string(),
            ],
            "#1821 pause and resume activity follow the ts sequence"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn weixin_transport_sends_pulse_then_final_text() {
        let shared = shared_mock();
        let options = LiveReplyOptions {
            activity_delay_ms: 10,
            activity_update_throttle_ms: 10,
            activity_max_updates: 2,
            format_activity: Arc::new(|_| "正在思考…".to_string()),
            ..LiveReplyOptions::default()
        };
        let mut reply = controller(
            &shared,
            Caps {
                pulse: Some(PulseLabel::Plain),
                stop: true,
                send_no_handle: true,
                ..Caps::default()
            },
            options,
        );
        reply.handle_event(&event("turn_started")).await;
        advance(12).await;
        reply.fire_due_timers().await;
        reply.handle_event(&event("model_request_started")).await;
        advance(12).await;
        reply.fire_due_timers().await;
        reply
            .handle_event(&text_event("assistant_text_delta", "hello"))
            .await;
        reply.flush_final().await;
        crate::negative_assert_eq!(
            recorded(&shared),
            vec![
                "pulse:\"正在思考…\"".to_string(),
                "pulse:\"正在思考…\"".to_string(),
                "send:\"hello\"".to_string(),
                "stopActivity".to_string(),
            ],
            "#1821 the weixin transport pulses activity then sends the final text"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_send_enters_the_fallback_continuation() {
        let shared = shared_mock();
        let mut reply = controller(
            &shared,
            Caps {
                edit: true,
                send_fail_once: true,
                ..Caps::default()
            },
            LiveReplyOptions::default(),
        );
        reply.handle_event(&event("turn_started")).await;
        reply
            .handle_event(&text_event("assistant_text_delta", "TO"))
            .await;
        // The initial timer hits the refused send: the segment goes
        // edit-disabled with an empty fallback prefix.
        advance(800).await;
        reply.fire_due_timers().await;
        reply.flush_final().await;
        crate::negative_assert_eq!(
            recorded(&shared),
            vec!["send:\"TO ▉\"".to_string(), "send:\"TO\"".to_string(),],
            "#1821 a refused send enters the fallback continuation"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn mark_aborted_then_clear_follow_the_ts_paths() {
        let shared = shared_mock();
        let mut reply = controller(
            &shared,
            Caps {
                edit: true,
                stop: true,
                clear: true,
                ..Caps::default()
            },
            LiveReplyOptions::default(),
        );
        reply.handle_event(&event("turn_started")).await;
        reply
            .handle_event(&text_event("assistant_text_delta", "ab"))
            .await;
        advance(800).await;
        reply.fire_due_timers().await;
        reply.mark_aborted().await;
        reply.clear().await;
        crate::negative_assert_eq!(
            recorded(&shared),
            vec![
                "stopActivity".to_string(),
                "send:\"ab ▉\"".to_string(),
                "stopActivity".to_string(),
                "edit:1:\"处理已中止，请重新发送或稍后重试。\"".to_string(),
                "clear:1".to_string(),
                "stopActivity".to_string(),
            ],
            "#1821 markAborted then clear follow the ts paths"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn clear_prefers_clear_over_delete_when_both_exist() {
        // TS `if (transport.clear) … else if (transport.delete) …` branches on
        // capability existence, so a successful `clear` must not also `delete`.
        let shared = shared_mock();
        let mut reply = controller(
            &shared,
            Caps {
                clear: true,
                delete: true,
                ..Caps::default()
            },
            LiveReplyOptions::default(),
        );
        reply.handle_event(&event("turn_started")).await;
        reply
            .handle_event(&text_event("assistant_text_delta", "hi"))
            .await;
        reply.flush_final().await;
        reply.clear().await;
        crate::negative_assert_eq!(
            recorded(&shared),
            vec!["send:\"hi\"".to_string(), "clear:1".to_string()],
            "#1821 clear must not fall through to delete after a successful clear"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn zero_max_message_length_disables_the_limit() {
        // TS treats `maxMessageLength = 0` as falsy ("no limit") in
        // `formatForTransport` / `splitForTransport` / `liveTextLimit`.
        let shared = shared_mock();
        let mut reply = controller(
            &shared,
            Caps {
                max_message_length: Some(0),
                ..Caps::default()
            },
            LiveReplyOptions::default(),
        );
        reply.handle_event(&event("turn_started")).await;
        reply
            .handle_event(&text_event("assistant_text_delta", "aaa bbb ccc"))
            .await;
        reply.flush_final().await;
        crate::negative_assert_eq!(
            recorded(&shared),
            vec!["send:\"aaa bbb ccc\"".to_string()],
            "#1821 a zero max message length must not split (TS `!max` path)"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn channel_submit_failed_appends_the_weixin_warning() {
        let shared = shared_mock();
        let mut reply = controller(&shared, Caps::default(), LiveReplyOptions::default());
        reply.handle_event(&event("turn_started")).await;
        reply.channel_submit_failed().await;
        reply.flush_final().await;
        crate::negative_assert_eq!(
            recorded(&shared),
            vec!["send:\"\n⚠️ Failed to process this message. Please retry.\n\"".to_string()],
            "#1821 channel submit failed appends the weixin warning"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_timer_parks_until_the_next_deadline() {
        let shared = shared_mock();
        let options = LiveReplyOptions {
            turn_timeout_ms: 10,
            ..LiveReplyOptions::default()
        };
        let mut reply = controller(&shared, Caps::default(), options);
        reply.handle_event(&event("turn_started")).await;
        // A paused runtime auto-advances to the deadline while the caller
        // awaits, which is exactly the weixin select! arm.
        reply.wait_for_timer().await;
        crate::negative_assert_eq!(
            reply.fire_due_timers().await,
            TimerFirings {
                turn_timed_out: true
            },
            "#1821 wait for timer parks until the next deadline"
        );
    }
}
