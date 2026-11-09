//! TS cc7c4e65: ImLiveReplyController and the Weixin reply transport/renderers.
use super::*;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::Instant;

pub(crate) const DEFAULT_CURSOR: &str = " ▉";
pub(crate) const DEFAULT_THROTTLE_MS: u64 = 2_000;
pub(crate) const DEFAULT_BUFFER_THRESHOLD: usize = 96;
pub(crate) const DEFAULT_INITIAL_THROTTLE_MS: u64 = 800;
pub(crate) const DEFAULT_INITIAL_BUFFER_THRESHOLD: usize = 24;
pub(crate) const WEIXIN_TIMEOUT_FINAL_TEXT: &str =
    "处理时间已超过上限，任务已停止。你可以调整需求后重新发送。";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivityKind {
    Thinking,
    Tool,
    Subagent,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Activity {
    pub kind: ActivityKind,
    pub text: String,
    pub elapsed_ms: u64,
    pub update_count: u64,
    pub detail: Option<String>,
}
pub(crate) trait LiveTransport: Send + Sync {
    fn send<'a>(&'a self, text: &'a str) -> ChannelCall<'a, Option<bool>>;
    fn edit<'a>(&'a self, _text: &'a str) -> ChannelCall<'a, Option<bool>> {
        Box::pin(async { Ok(None) })
    }
    fn has_edit(&self) -> bool {
        false
    }
    fn has_pulse_activity(&self) -> bool {
        false
    }
    /// None is a successful void pulse when has_pulse_activity is true.
    fn pulse_activity<'a>(&'a self, _activity: &'a Activity) -> ChannelCall<'a, Option<bool>> {
        Box::pin(async { Ok(None) })
    }
    fn stop_activity(&self) -> ChannelCall<'_, Option<bool>> {
        Box::pin(async { Ok(None) })
    }
    fn max_message_length(&self) -> Option<usize> {
        None
    }
    fn format_text(&self, text: &str) -> String {
        text.into()
    }
}

/// The owner supplies credentials/context, queueing, and its existing typing RPC.
pub(crate) trait WeixinReplyIo: Send + Sync {
    /// WeixinChannel.ts:881-883: both a connected client and a nonempty context token.
    fn typing_available(&self) -> bool;
    fn send_reply<'a>(&'a self, text: &'a str, queue_on_failure: bool) -> ChannelCall<'a, bool>;
    fn send_typing_if_possible(&self) -> ChannelCall<'_, ()>;
}
pub(crate) struct WeixinLiveTransport {
    io: Arc<dyn WeixinReplyIo>,
}
impl WeixinLiveTransport {
    pub fn new(io: Arc<dyn WeixinReplyIo>) -> Self {
        Self { io }
    }
    pub async fn send_typing_if_possible(&self) {
        if !self.io.typing_available() {
            return;
        }
        if let Err(error) = self.io.send_typing_if_possible().await {
            eprintln!("weixin: sendTyping failed: {}", error.message);
        }
    }
}
impl LiveTransport for WeixinLiveTransport {
    fn has_pulse_activity(&self) -> bool {
        true
    }
    fn send<'a>(&'a self, text: &'a str) -> ChannelCall<'a, Option<bool>> {
        Box::pin(async move {
            self.io.send_reply(text, true).await?;
            Ok(None)
        })
    }
    fn pulse_activity<'a>(&'a self, activity: &'a Activity) -> ChannelCall<'a, Option<bool>> {
        Box::pin(async move {
            // WeixinChannel.ts:793 ignores delivery's boolean and still sends typing.
            if let Err(error) = self.io.send_reply(&activity.text, false).await {
                eprintln!("weixin: activity failed: {}", error.message);
            }
            self.send_typing_if_possible().await;
            Ok(Some(true))
        })
    }
    fn stop_activity(&self) -> ChannelCall<'_, Option<bool>> {
        Box::pin(async { Ok(Some(true)) })
    }
}

#[derive(Clone)]
pub(crate) struct LiveReplyOptions {
    pub throttle_ms: u64,
    pub buffer_threshold: usize,
    pub initial_throttle_ms: u64,
    pub initial_buffer_threshold: usize,
    pub turn_timeout_ms: u64,
    pub activity_delay_ms: i64,
    pub activity_update_throttle_ms: u64,
    pub activity_max_updates: u64,
    pub activity_ttl_ms: u64,
    pub cursor: String,
    pub activity_only_final_text: String,
    pub timeout_final_text: String,
    pub abort_final_text: String,
    pub format_tool_error: fn(&Value) -> String,
    pub format_error: fn(&Value) -> String,
    pub format_activity: fn(ActivityKind, u64) -> String,
}
impl Default for LiveReplyOptions {
    fn default() -> Self {
        Self {
            throttle_ms: DEFAULT_THROTTLE_MS,
            buffer_threshold: DEFAULT_BUFFER_THRESHOLD,
            initial_throttle_ms: DEFAULT_INITIAL_THROTTLE_MS,
            initial_buffer_threshold: DEFAULT_INITIAL_BUFFER_THRESHOLD,
            turn_timeout_ms: 600_000,
            activity_delay_ms: 2_500,
            activity_update_throttle_ms: 10_000,
            activity_max_updates: 6,
            activity_ttl_ms: 120_000,
            cursor: DEFAULT_CURSOR.into(),
            activity_only_final_text: "处理完成，但没有可见回复。".into(),
            timeout_final_text: "处理超时，请重新发送或稍后重试。".into(),
            abort_final_text: "处理已中止，请重新发送或稍后重试。".into(),
            format_tool_error: tool_error,
            format_error: default_error,
            format_activity: default_activity,
        }
    }
}
impl LiveReplyOptions {
    /// WeixinChannel.ts:666-676, including the disabled default turn watchdog.
    pub fn weixin() -> Self {
        Self {
            turn_timeout_ms: 0,
            activity_delay_ms: 600_000,
            activity_update_throttle_ms: 600_000,
            activity_max_updates: 9_007_199_254_740_991,
            activity_ttl_ms: 9_007_199_254_740_991,
            timeout_final_text: WEIXIN_TIMEOUT_FINAL_TEXT.into(),
            format_activity: format_weixin_activity,
            ..Self::default()
        }
    }
}

struct Segment {
    text: String,
    handle: bool,
    first_text_at: Option<Instant>,
    last_visible_text: String,
    last_visible_final_text: String,
    edit_disabled: bool,
    fallback_prefix: String,
    final_text: bool,
    activity_kind: ActivityKind,
    activity_started_at: Instant,
    activity_armed: bool,
    activity_updates: u64,
    activity_visible: bool,
    activity_disabled: bool,
    activity_detail: Option<String>,
}
impl Default for Segment {
    fn default() -> Self {
        Self {
            text: String::new(),
            handle: false,
            first_text_at: None,
            last_visible_text: String::new(),
            last_visible_final_text: String::new(),
            edit_disabled: false,
            fallback_prefix: String::new(),
            final_text: false,
            activity_kind: ActivityKind::Thinking,
            activity_started_at: Instant::now(),
            activity_armed: false,
            activity_updates: 0,
            activity_visible: false,
            activity_disabled: false,
            activity_detail: None,
        }
    }
}
#[derive(Clone, Copy)]
enum Timer {
    Text,
    Turn,
    ActivityDelay,
    ActivityUpdate,
}
struct ReplyState {
    owner: std::sync::Weak<Mutex<ReplyState>>,
    transport: Arc<dyn LiveTransport>,
    options: LiveReplyOptions,
    segment: Segment,
    closed: bool,
    visible_failure: bool,
    text_timer: Option<JoinHandle<()>>,
    turn_timer: Option<JoinHandle<()>>,
    activity_delay_timer: Option<JoinHandle<()>>,
    activity_update_timer: Option<JoinHandle<()>>,
}
pub(crate) struct LiveReplyController {
    state: Arc<Mutex<ReplyState>>,
}
impl LiveReplyController {
    pub fn new(transport: Arc<dyn LiveTransport>) -> Self {
        Self::with_options(transport, LiveReplyOptions::default())
    }
    pub fn with_options(transport: Arc<dyn LiveTransport>, options: LiveReplyOptions) -> Self {
        Self {
            state: Arc::new_cyclic(|owner| {
                Mutex::new(ReplyState {
                    owner: owner.clone(),
                    transport,
                    options,
                    segment: Segment::default(),
                    closed: false,
                    visible_failure: false,
                    text_timer: None,
                    turn_timer: None,
                    activity_delay_timer: None,
                    activity_update_timer: None,
                })
            }),
        }
    }
    // The same mutex serializes public calls and timer flushes (TS runExclusive:615).
    pub async fn handle_event(&mut self, event: &Value) -> Result<()> {
        let mut state = self.state.lock().await;
        if state.closed {
            return Ok(());
        }
        state.arm_turn_timer(false);
        match text(event, "type").unwrap_or("") {
            "turn_started" => {
                state.visible_failure = false;
                state.arm_turn_timer(true);
                state.mark_activity(ActivityKind::Thinking, None);
            }
            "model_request_started" => state.mark_activity(ActivityKind::Thinking, None),
            "assistant_thinking_delta" => {
                state.mark_activity(ActivityKind::Thinking, text(event, "text"))
            }
            "assistant_text_delta" => state.append(text(event, "text").unwrap_or("")).await,
            "tool_call_started" => {
                if state.transport.has_edit()
                    || state.segment.handle
                    || !state.segment.last_visible_final_text.is_empty()
                {
                    state.flush_segment(false).await;
                } else {
                    state.cancel(Timer::Text);
                }
                state.mark_activity(ActivityKind::Tool, text(event, "name"));
            }
            "tool_call_finished" => {
                if event["ok"] != true {
                    let warning = (state.options.format_tool_error)(event);
                    state.append(&warning).await;
                } else {
                    state.mark_activity(ActivityKind::Thinking, None);
                }
            }
            "agent_status" => state.agent_status(event).await,
            "elicitation_request" => {
                state.cancel_activity();
                state.stop_activity().await;
            }
            "error" if !state.visible_failure => {
                let message = (state.options.format_error)(event);
                state.append(&message).await;
            }
            _ => {}
        }
        Ok(())
    }
    pub async fn mark_timed_out(&mut self) -> Result<()> {
        let mut state = self.state.lock().await;
        let message = state.options.timeout_final_text.clone();
        state.terminal(&message).await;
        Ok(())
    }
    pub async fn mark_aborted(&mut self) -> Result<()> {
        let mut state = self.state.lock().await;
        let message = state.options.abort_final_text.clone();
        state.terminal(&message).await;
        Ok(())
    }
    pub async fn flush_final(&mut self) -> Result<()> {
        let mut state = self.state.lock().await;
        if !state.closed {
            state.cancel(Timer::Turn);
            state.flush_segment(true).await;
            state.closed = true;
            state.stop_activity().await;
        }
        Ok(())
    }
    pub async fn pause_activity(&mut self) -> Result<()> {
        let mut state = self.state.lock().await;
        state.cancel_activity();
        state.stop_activity().await;
        Ok(())
    }
    pub async fn resume_activity(&mut self, kind: ActivityKind, immediate: bool) -> Result<()> {
        let mut state = self.state.lock().await;
        if state.closed {
            return Ok(());
        }
        state.cancel_activity();
        state.segment.activity_kind = kind;
        state.segment.activity_started_at = Instant::now();
        state.segment.activity_armed = true;
        state.segment.activity_updates = 0;
        if immediate {
            state.flush_activity(true).await;
        } else {
            state.schedule_activity_delay();
        }
        Ok(())
    }
}
impl Drop for LiveReplyController {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.try_lock() {
            state.cancel(Timer::Text);
            state.cancel(Timer::Turn);
            state.cancel_activity();
        }
    }
}
impl ReplyState {
    fn timer(&mut self, timer: Timer) -> &mut Option<JoinHandle<()>> {
        match timer {
            Timer::Text => &mut self.text_timer,
            Timer::Turn => &mut self.turn_timer,
            Timer::ActivityDelay => &mut self.activity_delay_timer,
            Timer::ActivityUpdate => &mut self.activity_update_timer,
        }
    }
    fn cancel(&mut self, timer: Timer) {
        if let Some(task) = self.timer(timer).take() {
            task.abort();
        }
    }
    fn cancel_activity(&mut self) {
        self.cancel(Timer::ActivityDelay);
        self.cancel(Timer::ActivityUpdate);
    }
    fn schedule(&mut self, timer: Timer, delay: u64) {
        if self.closed || self.timer(timer).is_some() {
            return;
        }
        let owner = self.owner.clone();
        let deadline = Instant::now() + Duration::from_millis(delay);
        *self.timer(timer) = Some(tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            if let Some(owner) = owner.upgrade() {
                let mut state = owner.lock().await;
                state.timer(timer).take();
                if state.closed {
                    return;
                }
                match timer {
                    Timer::Text => state.flush_current(false, false, false).await,
                    Timer::Turn => {
                        let message = state.options.timeout_final_text.clone();
                        state.terminal(&message).await;
                    }
                    Timer::ActivityDelay => state.flush_activity(true).await,
                    Timer::ActivityUpdate => state.flush_activity(false).await,
                }
            }
        }));
    }
    fn arm_turn_timer(&mut self, force: bool) {
        if self.options.turn_timeout_ms == 0 {
            return;
        }
        if force {
            self.cancel(Timer::Turn);
        }
        self.schedule(Timer::Turn, self.options.turn_timeout_ms);
    }
    async fn stop_activity(&self) {
        if let Err(error) = self.transport.stop_activity().await {
            Self::report("stopActivity", &error);
        }
    }
    fn report(phase: &str, error: &super::super::super::super::RouteError) {
        eprintln!("weixin: live reply {phase} failed: {}", error.message);
    }
    async fn agent_status(&mut self, event: &Value) {
        let name = text(event, "event").unwrap_or("");
        let detail = &event["detail"];
        let detail_failure = detail["visible"] != false && detail["severity"] == "error";
        let known_failure = matches!(
            name,
            "model_empty_response_exhausted"
                | "max_turns_reached"
                | "max_output_recovery_exhausted"
                | "model_request_failed"
                | "tool_call_recovery_exhausted"
                | "tool_error_loop"
                | "lifecycle_blocked"
                | "turn_failed"
                | "turn_timeout"
                | "gateway_submit_failed"
                | "session_busy"
                | "gateway_bridge_error"
                | "gateway_stream_ended_without_completion"
                | "web_http_request_failed"
                | "project_unavailable"
                | "config_invalid"
                | "gateway_unavailable"
                | "channel_submit_failed"
                | "subagent_failed"
                | "content_filter_stop"
                | "unknown_finish_reason"
        );
        if detail["visible"] != false && (known_failure || detail_failure) {
            self.visible_failure = true;
        }
        let default = match name {
            "model_empty_response_exhausted" => Some("The model returned empty content repeatedly, so this turn has stopped. Try again later or increase max output tokens."),
            "max_turns_reached" => Some("Reached the maximum number of turns, so this turn has stopped. Increase maxTurns or split the task into smaller steps and try again."),
            _ => None,
        };
        if known_failure
            || detail_failure
            || matches!(name, "structured_output_completed" | "turn_aborted")
        {
            if let Some(message) = text(detail, "message").or(default) {
                if !message.is_empty() || default.is_some() {
                    self.append(&format!("\n⚠️ {message}\n")).await;
                }
            }
        } else if name == "subagent_completed" {
            self.mark_activity(ActivityKind::Thinking, None);
        } else if name.starts_with("subagent_") {
            self.mark_activity(ActivityKind::Subagent, text(detail, "subagentType"));
        }
    }
    fn suppress_activity(&self) -> bool {
        if !self.transport.has_edit() && self.transport.has_pulse_activity() {
            return false;
        }
        !self.segment.text.trim().is_empty() || !self.segment.last_visible_final_text.is_empty()
    }
    fn mark_activity(&mut self, kind: ActivityKind, detail: Option<&str>) {
        if (!self.transport.has_edit() && !self.transport.has_pulse_activity())
            || self.segment.activity_disabled
            || self.suppress_activity()
        {
            return;
        }
        let changed = self.segment.activity_kind != kind;
        if changed || !self.segment.activity_armed {
            self.cancel_activity();
            self.segment.activity_kind = kind;
            self.segment.activity_started_at = Instant::now();
            self.segment.activity_armed = true;
            self.segment.activity_updates = 0;
        }
        if let Some(detail) = detail.filter(|s| !s.trim().is_empty()) {
            self.segment.activity_detail = Some(utf16_prefix(detail.trim(), 160).into());
        }
        if self.segment.activity_visible && !changed {
            self.schedule_activity_update();
        } else {
            self.schedule_activity_delay();
        }
    }
    fn schedule_activity_delay(&mut self) {
        if (self.transport.has_edit() || self.transport.has_pulse_activity())
            && !self.segment.activity_disabled
            && !self.suppress_activity()
            && self.options.activity_delay_ms >= 0
        {
            self.schedule(Timer::ActivityDelay, self.options.activity_delay_ms as u64);
        }
    }
    fn schedule_activity_update(&mut self) {
        if !self.segment.activity_disabled
            && self.segment.activity_visible
            && !self.suppress_activity()
            && self.segment.activity_updates < self.options.activity_max_updates
            && elapsed_ms(self.segment.activity_started_at) < self.options.activity_ttl_ms
        {
            self.schedule(
                Timer::ActivityUpdate,
                self.options.activity_update_throttle_ms,
            );
        }
    }
    async fn flush_activity(&mut self, force: bool) {
        if self.closed
            || self.segment.activity_disabled
            || self.suppress_activity()
            || self.segment.activity_updates >= self.options.activity_max_updates
        {
            return;
        }
        let elapsed = elapsed_ms(self.segment.activity_started_at);
        if elapsed >= self.options.activity_ttl_ms {
            return;
        }
        let activity = Activity {
            kind: self.segment.activity_kind,
            text: (self.options.format_activity)(self.segment.activity_kind, elapsed),
            elapsed_ms: elapsed,
            update_count: self.segment.activity_updates + 1,
            detail: self.segment.activity_detail.clone(),
        };
        if self.transport.has_pulse_activity() {
            match self.transport.pulse_activity(&activity).await {
                Ok(Some(false)) => {
                    self.segment.activity_disabled = true;
                    return;
                }
                Ok(_) => {
                    self.segment.activity_visible = true;
                    self.segment.activity_updates += 1;
                    self.schedule_activity_update();
                    return;
                }
                Err(error) => {
                    Self::report("activity", &error);
                    self.segment.activity_disabled = true;
                    return;
                }
            }
        }
        if !self.transport.has_edit() {
            return;
        }
        let formatted = self.format(&format!("{}{}", activity.text, self.options.cursor));
        if !force && formatted == self.segment.last_visible_text {
            self.segment.activity_updates += 1;
            self.schedule_activity_update();
            return;
        }
        let ok = if self.segment.handle {
            match self.transport.edit(&formatted).await {
                Ok(value) if value != Some(false) => true,
                result => {
                    if let Err(error) = result {
                        Self::report("edit", &error);
                    }
                    self.segment.edit_disabled = true;
                    self.segment.fallback_prefix.clear();
                    false
                }
            }
        } else {
            match self.transport.send(&formatted).await {
                Ok(Some(true)) => {
                    self.segment.handle = true;
                    true
                }
                Ok(None) => {
                    self.segment.edit_disabled = true;
                    false
                }
                Ok(Some(false)) => false,
                Err(error) => {
                    Self::report("send", &error);
                    false
                }
            }
        };
        if ok {
            self.segment.last_visible_text = formatted;
            self.segment.activity_visible = true;
            self.segment.activity_updates += 1;
            self.schedule_activity_update();
        } else {
            self.segment.activity_disabled = true;
        }
    }
    async fn append(&mut self, value: &str) {
        if value.is_empty() {
            return;
        }
        if self.transport.has_edit() {
            self.cancel_activity();
            self.stop_activity().await;
        }
        let limit = if self.transport.has_edit() {
            self.transport
                .max_message_length()
                .filter(|n| *n > 0)
                .map(|n| n.saturating_sub(utf16_len(&self.options.cursor)).max(1))
        } else {
            None
        };
        let mut rest = value;
        while !rest.is_empty() {
            let available = limit.map(|n| n.saturating_sub(utf16_len(&self.segment.text)));
            if available == Some(0) {
                self.flush_segment(true).await;
                continue;
            }
            let piece = available.map(|n| utf16_prefix(rest, n)).unwrap_or(rest);
            if piece.is_empty() {
                // Rust strings cannot contain half a surrogate: move the whole scalar.
                if !self.segment.text.is_empty() {
                    self.flush_segment(true).await;
                    continue;
                }
                let size = rest.chars().next().unwrap().len_utf8();
                self.append_piece(&rest[..size]).await;
                rest = &rest[size..];
            } else {
                self.append_piece(piece).await;
                rest = &rest[piece.len()..];
            }
            if !rest.is_empty() {
                self.flush_segment(true).await;
            }
        }
    }
    async fn append_piece(&mut self, value: &str) {
        self.segment.text.push_str(value);
        let started = *self.segment.first_text_at.get_or_insert_with(Instant::now);
        if !self.transport.has_edit() || self.segment.edit_disabled {
            return;
        }
        if self.segment.last_visible_final_text.is_empty() {
            if utf16_len(&self.segment.text) >= self.options.initial_buffer_threshold {
                self.flush_current(false, true, false).await;
            } else {
                self.schedule(
                    Timer::Text,
                    self.options
                        .initial_throttle_ms
                        .saturating_sub(elapsed_ms(started)),
                );
            }
        } else if utf16_len(&self.segment.text)
            .saturating_sub(utf16_len(&self.segment.last_visible_final_text))
            >= self.options.buffer_threshold
        {
            self.flush_current(false, true, false).await;
        } else {
            self.schedule(Timer::Text, self.options.throttle_ms);
        }
    }
    async fn flush_segment(&mut self, activity_only: bool) {
        self.cancel(Timer::Text);
        self.cancel_activity();
        self.flush_current(true, true, activity_only).await;
        if !self.segment.text.trim().is_empty()
            || !self.segment.last_visible_final_text.trim().is_empty()
            || (activity_only && !self.segment.last_visible_text.trim().is_empty())
        {
            self.segment = Segment::default();
        }
    }
    async fn flush_current(&mut self, final_text: bool, force: bool, activity_only: bool) {
        self.cancel(Timer::Text);
        let value = self.segment.text.clone();
        if value.trim().is_empty() {
            if final_text && activity_only && self.segment.activity_visible {
                self.finalize_activity_only().await;
            }
            return;
        }
        if !final_text && (!self.transport.has_edit() || self.segment.edit_disabled) {
            return;
        }
        if final_text && !self.transport.has_edit() && !self.segment.handle {
            self.send_final_chunks(&value, false).await;
            self.segment.final_text = true;
            return;
        }
        if self.segment.edit_disabled {
            if final_text {
                self.fallback_continuation(&value).await;
                self.segment.final_text = true;
            }
            return;
        }
        let visible = if final_text {
            value.clone()
        } else {
            format!("{value}{}", self.options.cursor)
        };
        let formatted = self.format(&visible);
        if !force && formatted == self.segment.last_visible_text {
            return;
        }
        if self.segment.handle && self.transport.has_edit() {
            match self.transport.edit(&formatted).await {
                Ok(result) if result != Some(false) => {
                    self.record_visible(formatted, value, final_text);
                }
                result => {
                    if let Err(error) = result {
                        Self::report("edit", &error);
                    }
                    self.enter_fallback().await;
                    if final_text {
                        self.fallback_continuation(&value).await;
                        self.segment.final_text = true;
                    }
                }
            }
            return;
        }
        if self.segment.handle {
            self.segment.edit_disabled = true;
            self.segment.fallback_prefix = self.segment.last_visible_final_text.clone();
            if final_text {
                self.fallback_continuation(&value).await;
                self.segment.final_text = true;
            }
            return;
        }
        match self.transport.send(&formatted).await {
            Ok(handle) if handle != Some(false) => {
                self.segment.handle = handle == Some(true);
                self.record_visible(formatted, value.clone(), final_text);
                if handle.is_none() || !self.transport.has_edit() {
                    self.segment.edit_disabled = true;
                    self.segment.fallback_prefix = value;
                }
            }
            result => {
                if let Err(error) = result {
                    Self::report("send", &error);
                }
                self.segment.edit_disabled = true;
                self.segment.fallback_prefix.clear();
                if final_text {
                    self.fallback_continuation(&value).await;
                    self.segment.final_text = true;
                }
            }
        }
    }
    fn record_visible(&mut self, visible: String, final_text: String, final_flag: bool) {
        self.segment.activity_visible = false;
        self.segment.last_visible_text = visible;
        self.segment.last_visible_final_text = final_text;
        self.segment.final_text = final_flag;
    }
    async fn enter_fallback(&mut self) {
        if self.segment.edit_disabled {
            return;
        }
        self.segment.edit_disabled = true;
        self.segment.fallback_prefix = self.segment.last_visible_final_text.clone();
        if self.segment.handle && self.transport.has_edit() && !self.options.cursor.is_empty() {
            if let Some(clean) = self
                .segment
                .last_visible_text
                .strip_suffix(&self.options.cursor)
            {
                let clean = clean.to_owned();
                match self.transport.edit(&clean).await {
                    Ok(result) if result != Some(false) => self.segment.last_visible_text = clean,
                    Err(error) => Self::report("edit", &error),
                    _ => {}
                }
            }
        }
    }
    async fn fallback_continuation(&mut self, final_text: &str) {
        let rest = if self.segment.fallback_prefix.is_empty() {
            final_text
        } else {
            final_text
                .strip_prefix(&self.segment.fallback_prefix)
                .map(str::trim_start)
                .unwrap_or(final_text)
        };
        if rest.trim().is_empty() {
            self.segment.last_visible_final_text = final_text.into();
            return;
        }
        if self.send_final_chunks(rest, true).await {
            self.segment.last_visible_final_text = final_text.into();
            self.segment.fallback_prefix = final_text.into();
        }
    }
    async fn send_final_chunks(&mut self, value: &str, continuation: bool) -> bool {
        for chunk in self.split(value) {
            match self.transport.send(&chunk).await {
                Ok(handle) if handle != Some(false) => {
                    if continuation || handle.is_some() {
                        self.segment.handle = handle == Some(true);
                    }
                    self.record_visible(chunk, value.into(), self.segment.final_text);
                    if continuation && handle == Some(true) && self.transport.has_edit() {
                        self.segment.edit_disabled = false;
                    }
                }
                Err(error) => {
                    Self::report("send", &error);
                    return false;
                }
                _ => return false,
            }
        }
        true
    }
    async fn finalize_activity_only(&mut self) {
        self.stop_activity().await;
        let formatted = self.format(&self.options.activity_only_final_text);
        if !self.segment.handle || !self.transport.has_edit() || self.segment.edit_disabled {
            if !self.transport.has_edit() && self.segment.activity_visible {
                match self.transport.send(&formatted).await {
                    Ok(handle) if handle != Some(false) => {
                        if handle.is_some() {
                            self.segment.handle = true;
                        }
                        self.segment.last_visible_text = formatted.clone();
                        self.segment.last_visible_final_text = formatted;
                        self.segment.final_text = true;
                    }
                    Err(error) => Self::report("send", &error),
                    _ => {}
                }
            }
            return;
        }
        match self.transport.edit(&formatted).await {
            Ok(result) if result != Some(false) => {
                self.record_visible(formatted.clone(), formatted, true)
            }
            result => {
                if let Err(error) = result {
                    Self::report("edit", &error);
                }
                self.segment.edit_disabled = true;
            }
        }
    }
    async fn terminal(&mut self, value: &str) {
        if self.closed {
            return;
        }
        self.cancel(Timer::Text);
        self.cancel(Timer::Turn);
        self.cancel_activity();
        self.stop_activity().await;
        let formatted = self.format(value);
        self.segment.text.clear();
        self.segment.activity_visible = false;
        self.segment.last_visible_final_text = formatted.clone();
        self.segment.final_text = true;
        if self.segment.handle && self.transport.has_edit() && !self.segment.edit_disabled {
            match self.transport.edit(&formatted).await {
                Ok(result) if result != Some(false) => {
                    self.segment.last_visible_text = formatted;
                    self.closed = true;
                    return;
                }
                Err(error) => Self::report("edit", &error),
                _ => {}
            }
            self.segment.edit_disabled = true;
        }
        if !self.segment.handle
            || self.segment.last_visible_text.trim().is_empty()
            || self.transport.has_edit()
        {
            match self.transport.send(&formatted).await {
                Ok(handle) if handle != Some(false) => {
                    if handle.is_some() {
                        self.segment.handle = true;
                    }
                    self.segment.last_visible_text = formatted;
                }
                Err(error) => Self::report("send", &error),
                _ => {}
            }
        }
        self.closed = true;
    }
    fn format(&self, value: &str) -> String {
        let formatted = self.transport.format_text(value);
        self.transport
            .max_message_length()
            .filter(|n| *n > 0)
            .map(|n| utf16_prefix(&formatted, n).into())
            .unwrap_or(formatted)
    }
    fn split(&self, value: &str) -> Vec<String> {
        let formatted = self.transport.format_text(value);
        let Some(max) = self.transport.max_message_length().filter(|n| *n > 0) else {
            return vec![formatted];
        };
        let mut rest = formatted.as_str();
        let mut chunks = Vec::new();
        while utf16_len(rest) > max {
            let prefix = utf16_prefix(rest, max);
            // JS lastIndexOf includes a delimiter at the max offset.
            let inclusive_end = prefix.len()
                + rest[prefix.len()..]
                    .chars()
                    .next()
                    .map(char::len_utf8)
                    .unwrap_or(0);
            let searchable = &rest[..inclusive_end];
            let split = searchable
                .rfind('\n')
                .filter(|n| utf16_len(&rest[..*n]) >= max.div_ceil(2))
                .or_else(|| {
                    searchable
                        .rfind(' ')
                        .filter(|n| utf16_len(&rest[..*n]) >= max.div_ceil(2))
                })
                .unwrap_or_else(|| {
                    if prefix.is_empty() {
                        rest.chars().next().unwrap().len_utf8()
                    } else {
                        prefix.len()
                    }
                });
            chunks.push(rest[..split].into());
            rest = rest[split..].trim_start_matches('\n');
        }
        if !rest.is_empty() {
            chunks.push(rest.into());
        }
        chunks
    }
}
fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}
fn utf16_prefix(value: &str, max: usize) -> &str {
    let mut units = 0;
    for (index, ch) in value.char_indices() {
        units += ch.len_utf16();
        if units > max {
            return &value[..index];
        }
    }
    value
}
fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u64::MAX as u128) as u64
}
fn tool_error(event: &Value) -> String {
    let name = text(event, "toolName")
        .or_else(|| text(event, "toolCallId"))
        .unwrap_or("undefined");
    format!("\n⚠️ {name} 执行失败\n")
}
fn default_error(event: &Value) -> String {
    let mut result = format!("\n❌ {}", text(event, "message").unwrap_or("undefined"));
    if let Some(provider) = event.get("providerError").filter(|v| !v.is_null()) {
        result.push_str("\nProvider error:");
        for key in ["provider", "status", "code", "message", "raw"] {
            if let Some(value) = provider.get(key) {
                if key == "status" || value.as_str().is_some_and(|s| !s.is_empty()) {
                    let value = value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string());
                    result.push_str(&format!("\n- {key}: {value}"));
                }
            }
        }
    }
    result.push('\n');
    result
}
fn default_activity(kind: ActivityKind, elapsed: u64) -> String {
    let base = match kind {
        ActivityKind::Thinking => "正在思考…",
        ActivityKind::Tool => "正在执行工具…",
        ActivityKind::Subagent => "正在处理子任务…",
    };
    if elapsed < 10_000 {
        base.into()
    } else {
        format!("{base}（{}s）", elapsed.saturating_add(500) / 1_000)
    }
}
pub(crate) fn render_weixin_event(event: &Value) -> Option<String> {
    match text(event, "type").unwrap_or("") {
        "assistant_text_delta" => Some(text(event, "text").unwrap_or("").into()),
        "assistant_thinking_delta" | "tool_call_started" => Some(String::new()),
        "tool_call_finished" => Some(
            if event["ok"] == true
                || text(event, "toolName").or_else(|| text(event, "toolCallId"))
                    == Some("send_attachment")
            {
                String::new()
            } else {
                tool_error(event)
            },
        ),
        "elicitation_request" => {
            let mut lines = Vec::new();
            for question in event["questions"].as_array().into_iter().flatten() {
                if let Some(header) = text(question, "header").filter(|s| !s.is_empty()) {
                    lines.push(format!("**{header}**"));
                }
                if let Some(question) = text(question, "question").filter(|s| !s.is_empty()) {
                    lines.push(question.into());
                }
                for (index, option) in question["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    lines.push(format!(
                        "{}. {}",
                        index + 1,
                        text(option, "label").unwrap_or("undefined")
                    ));
                }
            }
            Some(if lines.is_empty() {
                String::new()
            } else {
                format!("\n{}\n", lines.join("\n"))
            })
        }
        "error" => Some(format!(
            "\n❌ {}\n",
            text(event, "message").unwrap_or("undefined")
        )),
        _ => None,
    }
}
pub(crate) fn format_weixin_activity(kind: ActivityKind, elapsed_ms: u64) -> String {
    let base = match kind {
        ActivityKind::Thinking => "仍在处理：正在分析和生成回复",
        ActivityKind::Tool => "仍在处理：正在执行工具",
        ActivityKind::Subagent => "仍在处理：正在处理子任务",
    };
    let elapsed = format_elapsed(elapsed_ms);
    if elapsed.is_empty() {
        base.into()
    } else {
        format!("{base}（已用时 {elapsed}）")
    }
}
pub(crate) fn format_elapsed(elapsed_ms: u64) -> String {
    let minutes = elapsed_ms / 60_000;
    if minutes == 0 {
        String::new()
    } else if minutes < 60 {
        format!("{minutes} 分钟")
    } else if minutes % 60 == 0 {
        format!("{} 小时", minutes / 60)
    } else {
        format!("{} 小时 {} 分钟", minutes / 60, minutes % 60)
    }
}

#[cfg(test)]
mod reply_tests {
    use super::*;
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Call {
        Send(String),
        Edit(String),
        Pulse(Activity),
        Stop,
        Reply(String, bool),
        Typing,
    }
    struct Fake {
        edit: bool,
        pulse: bool,
        calls: StdMutex<Vec<Call>>,
        sends: StdMutex<VecDeque<Result<Option<bool>>>>,
        edits: StdMutex<VecDeque<Result<Option<bool>>>>,
        pulses: StdMutex<VecDeque<Result<Option<bool>>>>,
        max: StdMutex<Option<usize>>,
        first_send_gate: StdMutex<Option<Arc<tokio::sync::Notify>>>,
    }
    impl Fake {
        fn new(edit: bool, pulse: bool) -> Arc<Self> {
            Arc::new(Self {
                edit,
                pulse,
                calls: StdMutex::new(Vec::new()),
                sends: StdMutex::new(VecDeque::new()),
                edits: StdMutex::new(VecDeque::new()),
                pulses: StdMutex::new(VecDeque::new()),
                max: StdMutex::new(None),
                first_send_gate: StdMutex::new(None),
            })
        }
        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }
        fn messages(&self) -> Vec<Call> {
            self.calls()
                .into_iter()
                .filter(|c| matches!(c, Call::Send(_) | Call::Edit(_)))
                .collect()
        }
        fn activity(&self) -> Vec<Activity> {
            self.calls()
                .into_iter()
                .filter_map(|c| {
                    if let Call::Pulse(a) = c {
                        Some(a)
                    } else {
                        None
                    }
                })
                .collect()
        }
    }
    impl LiveTransport for Fake {
        fn has_edit(&self) -> bool {
            self.edit
        }
        fn has_pulse_activity(&self) -> bool {
            self.pulse
        }
        fn max_message_length(&self) -> Option<usize> {
            *self.max.lock().unwrap()
        }
        fn send<'a>(&'a self, text: &'a str) -> ChannelCall<'a, Option<bool>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(Call::Send(text.into()));
                let gate = self.first_send_gate.lock().unwrap().take();
                if let Some(gate) = gate {
                    gate.notified().await;
                }
                self.sends
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Ok(if self.edit { Some(true) } else { None }))
            })
        }
        fn edit<'a>(&'a self, text: &'a str) -> ChannelCall<'a, Option<bool>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(Call::Edit(text.into()));
                self.edits
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Ok(Some(true)))
            })
        }
        fn pulse_activity<'a>(&'a self, activity: &'a Activity) -> ChannelCall<'a, Option<bool>> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .unwrap()
                    .push(Call::Pulse(activity.clone()));
                self.pulses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Ok(Some(true)))
            })
        }
        fn stop_activity(&self) -> ChannelCall<'_, Option<bool>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(Call::Stop);
                Ok(None)
            })
        }
    }
    async fn append(c: &mut LiveReplyController, text: &str) {
        c.handle_event(&json!({"type":"assistant_text_delta","text":text}))
            .await
            .unwrap();
    }
    async fn advance(ms: u64) {
        tokio::time::advance(Duration::from_millis(ms)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    // ImLiveReplyController.ts:362-387 appendToCurrentSegment, :228 flushFinal (REPORT 114,120,123).
    #[tokio::test(start_paused = true)]
    async fn reply_initial_24_pending_96_cursor_and_final() {
        let io = Fake::new(true, false);
        let mut c = LiveReplyController::new(io.clone());
        append(&mut c, &"a".repeat(23)).await;
        assert!(io.messages().is_empty());
        append(&mut c, "b").await;
        let prefix = format!("{}b", "a".repeat(23));
        assert_eq!(
            io.messages(),
            vec![Call::Send(format!("{prefix}{DEFAULT_CURSOR}"))]
        );
        append(&mut c, &"c".repeat(95)).await;
        assert_eq!(io.messages().len(), 1);
        append(&mut c, "d").await;
        let final_text = format!("{prefix}{}d", "c".repeat(95));
        assert_eq!(
            io.messages()[1],
            Call::Edit(format!("{final_text}{DEFAULT_CURSOR}"))
        );
        c.flush_final().await.unwrap();
        assert_eq!(io.messages()[2], Call::Edit(final_text));
        c.flush_final().await.unwrap();
        advance(610_000).await;
        assert_eq!(io.messages().len(), 3);
    }

    // ImLiveReplyController.ts:405-416 initial 800ms / subsequent 2000ms timers (REPORT 123).
    #[tokio::test(start_paused = true)]
    async fn reply_initial_800_edit_2000_timers() {
        let io = Fake::new(true, false);
        let mut c = LiveReplyController::new(io.clone());
        append(&mut c, "短句").await;
        advance(799).await;
        assert!(io.messages().is_empty());
        advance(1).await;
        assert_eq!(io.messages(), vec![Call::Send("短句 ▉".into())]);
        append(&mut c, "接续").await;
        advance(1999).await;
        assert_eq!(io.messages().len(), 1);
        advance(1).await;
        assert_eq!(io.messages()[1], Call::Edit("短句接续 ▉".into()));
        c.flush_final().await.unwrap();
        assert_eq!(io.messages()[2], Call::Edit("短句接续".into()));
    }

    // ImLiveReplyController.ts:398 liveTextLimit, :332 append, :908 splitForTransport (REPORT 123).
    #[tokio::test(start_paused = true)]
    async fn reply_utf16_limit_cursor_and_final_chunks() {
        let io = Fake::new(true, false);
        *io.max.lock().unwrap() = Some(6);
        let mut c = LiveReplyController::with_options(
            io.clone(),
            LiveReplyOptions {
                initial_buffer_threshold: 4,
                ..Default::default()
            },
        );
        append(&mut c, "😀😀😀😀").await;
        c.flush_final().await.unwrap();
        assert_eq!(
            io.messages(),
            vec![
                Call::Send("😀😀 ▉".into()),
                Call::Edit("😀😀".into()),
                Call::Send("😀😀 ▉".into()),
                Call::Edit("😀😀".into())
            ]
        );
        let io = Fake::new(false, false);
        *io.max.lock().unwrap() = Some(8);
        let mut c = LiveReplyController::new(io.clone());
        append(&mut c, "abcde\nfghij klmnop").await;
        assert!(io.messages().is_empty());
        c.flush_final().await.unwrap();
        assert_eq!(
            io.messages(),
            vec![
                Call::Send("abcde".into()),
                Call::Send("fghij".into()),
                Call::Send(" klmnop".into())
            ]
        );
    }

    // ImLiveReplyController.ts:821 enterFallbackMode, :844 stripCursorBestEffort, :860 continuation (REPORT 123).
    #[tokio::test(start_paused = true)]
    async fn reply_edit_false_sends_continuation_without_duplicate() {
        let io = Fake::new(true, false);
        let mut c = LiveReplyController::new(io.clone());
        let prefix = "首".repeat(24);
        append(&mut c, &prefix).await;
        io.edits.lock().unwrap().push_back(Ok(Some(false)));
        let rest = "后".repeat(96);
        append(&mut c, &rest).await;
        assert_eq!(
            io.messages(),
            vec![
                Call::Send(format!("{prefix} ▉")),
                Call::Edit(format!("{prefix}{rest} ▉")),
                Call::Edit(prefix.clone())
            ]
        );
        append(&mut c, "尾").await;
        c.flush_final().await.unwrap();
        c.flush_final().await.unwrap();
        assert_eq!(io.messages()[3], Call::Send(format!("{rest}尾")));
        assert_eq!(io.messages().len(), 4);
    }

    // ImLiveReplyController.ts:659-737 send/edit errors and undefined handles (REPORT 115,123).
    #[tokio::test(start_paused = true)]
    async fn reply_undefined_handle_edit_error_keep_prefix() {
        let io = Fake::new(true, false);
        io.sends.lock().unwrap().push_back(Ok(None));
        let mut c = LiveReplyController::new(io.clone());
        let prefix = "x".repeat(24);
        append(&mut c, &prefix).await;
        append(&mut c, "  remaining").await;
        c.flush_final().await.unwrap();
        assert_eq!(
            io.messages(),
            vec![
                Call::Send(format!("{prefix} ▉")),
                Call::Send("remaining".into())
            ]
        );
        let io = Fake::new(true, false);
        let mut c = LiveReplyController::new(io.clone());
        append(&mut c, &prefix).await;
        io.edits
            .lock()
            .unwrap()
            .push_back(Err(failure("edit unavailable")));
        append(&mut c, " tail").await;
        c.flush_final().await.unwrap();
        assert_eq!(io.messages().last(), Some(&Call::Send("tail".into())));
        assert_eq!(io.messages()[2], Call::Edit(prefix));
    }

    // ImLiveReplyController.ts:196 tool_call_finished; weixin-render.ts:11 (REPORT 120,154).
    #[tokio::test(start_paused = true)]
    async fn reply_tool_failure_warning_success_activity() {
        let io = Fake::new(false, true);
        let mut c = LiveReplyController::new(io.clone());
        c.handle_event(&json!({"type":"tool_call_finished","ok":false,"toolName":"read_file","toolCallId":"id"})).await.unwrap();
        c.handle_event(&json!({"type":"tool_call_finished","ok":true}))
            .await
            .unwrap();
        advance(2500).await;
        assert_eq!(io.activity()[0].kind, ActivityKind::Thinking);
        c.flush_final().await.unwrap();
        assert_eq!(
            io.messages(),
            vec![Call::Send("\n⚠️ read_file 执行失败\n".into())]
        );
    }

    // ImLiveReplyController.ts:299 handleAgentStatus, :209 error deduplication, :174 turn reset (REPORT 120).
    #[tokio::test(start_paused = true)]
    async fn reply_visible_failure_suppresses_error_turn_started_resets() {
        let io = Fake::new(false, false);
        let mut c = LiveReplyController::new(io.clone());
        c.handle_event(
            &json!({"type":"agent_status","event":"turn_failed","detail":{"message":"失败原因"}}),
        )
        .await
        .unwrap();
        c.handle_event(&json!({"type":"error","message":"重复错误"}))
            .await
            .unwrap();
        c.handle_event(&json!({"type":"turn_started"}))
            .await
            .unwrap();
        c.handle_event(&json!({"type":"error","message":"新错误"}))
            .await
            .unwrap();
        c.flush_final().await.unwrap();
        assert_eq!(
            io.messages(),
            vec![Call::Send("\n⚠️ 失败原因\n\n❌ 新错误\n".into())]
        );
    }

    // ImLiveReplyController.ts:87 VISIBLE_FAILURE_STATUS_EVENTS and :299 handleAgentStatus (REPORT 120).
    #[tokio::test(start_paused = true)]
    async fn reply_all_failure_statuses_and_warning_severity() {
        let names = [
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
        for name in names {
            let io = Fake::new(false, false);
            let mut c = LiveReplyController::new(io.clone());
            c.handle_event(&json!({"type":"agent_status","event":name,"detail":{"message":name}}))
                .await
                .unwrap();
            c.handle_event(&json!({"type":"error","message":"duplicate"}))
                .await
                .unwrap();
            c.flush_final().await.unwrap();
            assert_eq!(
                io.messages(),
                vec![Call::Send(format!("\n⚠️ {name}\n"))],
                "{name}"
            );
        }
        for name in ["structured_output_completed", "turn_aborted"] {
            let io = Fake::new(false, false);
            let mut c = LiveReplyController::new(io.clone());
            c.handle_event(
                &json!({"type":"agent_status","event":name,"detail":{"message":"notice"}}),
            )
            .await
            .unwrap();
            c.handle_event(&json!({"type":"error","message":"error"}))
                .await
                .unwrap();
            c.flush_final().await.unwrap();
            assert_eq!(
                io.messages(),
                vec![Call::Send("\n⚠️ notice\n\n❌ error\n".into())]
            );
        }
        let io = Fake::new(false, false);
        let mut c = LiveReplyController::new(io.clone());
        c.handle_event(&json!({"type":"agent_status","event":"custom","detail":{"severity":"error","message":"visible"}})).await.unwrap();
        c.handle_event(&json!({"type":"error","message":"hidden"}))
            .await
            .unwrap();
        c.flush_final().await.unwrap();
        assert_eq!(io.messages(), vec![Call::Send("\n⚠️ visible\n".into())]);
    }

    // ImLiveReplyController.ts:300 visible=false and :305-312 fallback warnings (REPORT 120).
    #[tokio::test(start_paused = true)]
    async fn reply_invisible_failure_error_and_status_defaults() {
        let io = Fake::new(false, false);
        let mut c = LiveReplyController::new(io.clone());
        c.handle_event(&json!({"type":"agent_status","event":"turn_failed","detail":{"visible":false,"message":"warning"}})).await.unwrap();
        c.handle_event(&json!({"type":"error","message":"still visible"}))
            .await
            .unwrap();
        c.handle_event(&json!({"type":"agent_status","event":"model_empty_response_exhausted"}))
            .await
            .unwrap();
        c.handle_event(&json!({"type":"agent_status","event":"max_turns_reached"}))
            .await
            .unwrap();
        c.flush_final().await.unwrap();
        assert_eq!(io.messages(), vec![Call::Send(concat!("\n⚠️ warning\n\n❌ still visible\n",
            "\n⚠️ The model returned empty content repeatedly, so this turn has stopped. Try again later or increase max output tokens.\n",
            "\n⚠️ Reached the maximum number of turns, so this turn has stopped. Increase maxTurns or split the task into smaller steps and try again.\n").into())]);
    }

    // ImLiveReplyController.ts:220 markTimedOut, :224 markAborted, :781 terminal (REPORT 121,122).
    #[tokio::test(start_paused = true)]
    async fn reply_timeout_abort_edit_first_send_without_handle() {
        for timed_out in [true, false] {
            for edit in [true, false] {
                let io = Fake::new(edit, false);
                let mut c = LiveReplyController::new(io.clone());
                let draft = "草".repeat(24);
                append(&mut c, &draft).await;
                if timed_out {
                    c.mark_timed_out().await.unwrap();
                } else {
                    c.mark_aborted().await.unwrap();
                }
                let terminal = if timed_out {
                    "处理超时，请重新发送或稍后重试。"
                } else {
                    "处理已中止，请重新发送或稍后重试。"
                };
                let expected = if edit {
                    vec![
                        Call::Send(format!("{draft} ▉")),
                        Call::Edit(terminal.into()),
                    ]
                } else {
                    vec![Call::Send(terminal.into())]
                };
                assert_eq!(io.messages(), expected);
                append(&mut c, "late").await;
                c.mark_aborted().await.unwrap();
                c.flush_final().await.unwrap();
                advance(610_000).await;
                assert_eq!(io.messages(), expected);
            }
        }
    }

    // WeixinChannel.ts:37 timeout override; ImLiveReplyController.ts:795 edit failure fallback (REPORT 121,122).
    #[tokio::test(start_paused = true)]
    async fn reply_weixin_timeout_override_failed_edit_sends_terminal() {
        let io = Fake::new(true, false);
        let mut c = LiveReplyController::with_options(io.clone(), LiveReplyOptions::weixin());
        append(&mut c, &"a".repeat(24)).await;
        io.edits.lock().unwrap().push_back(Ok(Some(false)));
        c.mark_timed_out().await.unwrap();
        assert_eq!(
            io.messages()[1],
            Call::Edit(WEIXIN_TIMEOUT_FINAL_TEXT.into())
        );
        assert_eq!(
            io.messages()[2],
            Call::Send(WEIXIN_TIMEOUT_FINAL_TEXT.into())
        );
        let io = Fake::new(false, false);
        let mut c = LiveReplyController::with_options(
            io.clone(),
            LiveReplyOptions {
                abort_final_text: "custom abort".into(),
                ..Default::default()
            },
        );
        c.mark_aborted().await.unwrap();
        assert_eq!(io.messages(), vec![Call::Send("custom abort".into())]);
    }

    // ImLiveReplyController.ts:420 armTurnTimer, :174 force reset; WeixinChannel.ts:666 (REPORT 121).
    #[tokio::test(start_paused = true)]
    async fn reply_turn_timeout_reset_weixin_default_disabled() {
        let io = Fake::new(false, false);
        let mut c = LiveReplyController::with_options(
            io.clone(),
            LiveReplyOptions {
                turn_timeout_ms: 1000,
                ..Default::default()
            },
        );
        c.handle_event(&json!({"type":"model_request_started"}))
            .await
            .unwrap();
        advance(900).await;
        c.handle_event(&json!({"type":"turn_started"}))
            .await
            .unwrap();
        advance(999).await;
        assert!(io.messages().is_empty());
        advance(1).await;
        assert_eq!(
            io.messages(),
            vec![Call::Send("处理超时，请重新发送或稍后重试。".into())]
        );
        let io = Fake::new(false, false);
        let mut c = LiveReplyController::with_options(io.clone(), LiveReplyOptions::weixin());
        append(&mut c, "pending").await;
        advance(610_000).await;
        assert!(io.messages().is_empty());
        c.flush_final().await.unwrap();
        assert_eq!(io.messages(), vec![Call::Send("pending".into())]);
    }

    // ImLiveReplyController.ts:251 pauseActivity, :256 resumeActivity (REPORT 124,125; 八项④).
    #[tokio::test(start_paused = true)]
    async fn reply_pause_resume_elicitation_failure_continues_turn() {
        let io = Fake::new(false, true);
        let mut c = LiveReplyController::new(io.clone());
        c.handle_event(&json!({"type":"turn_started"}))
            .await
            .unwrap();
        c.handle_event(&json!({"type":"elicitation_request","questions":[]}))
            .await
            .unwrap();
        advance(2500).await;
        assert!(io.activity().is_empty());
        assert!(io.messages().is_empty());
        // The owner sends the question; its failure does not enter handle_event.
        io.sends
            .lock()
            .unwrap()
            .push_back(Err(failure("question delivery failed")));
        assert!(io.send("question").await.is_err());
        c.resume_activity(ActivityKind::Tool, false).await.unwrap();
        advance(2499).await;
        assert!(io.activity().is_empty());
        advance(1).await;
        assert_eq!(io.activity()[0].kind, ActivityKind::Tool);
        c.pause_activity().await.unwrap();
        let count = io.activity().len();
        advance(20_000).await;
        assert_eq!(io.activity().len(), count);
        c.resume_activity(ActivityKind::Thinking, true)
            .await
            .unwrap();
        assert_eq!(io.activity().last().unwrap().kind, ActivityKind::Thinking);
        append(&mut c, "继续完成").await;
        c.flush_final().await.unwrap();
        assert_eq!(io.messages().last(), Some(&Call::Send("继续完成".into())));
    }

    // ImLiveReplyController.ts:432-573 delay, update throttle, max updates, TTL (REPORT 116,119,125).
    #[tokio::test(start_paused = true)]
    async fn reply_activity_delay_throttle_max_ttl_disable() {
        let io = Fake::new(false, true);
        io.pulses.lock().unwrap().push_back(Ok(None));
        let options = LiveReplyOptions {
            activity_delay_ms: 100,
            activity_update_throttle_ms: 200,
            activity_max_updates: 2,
            activity_ttl_ms: 1000,
            ..Default::default()
        };
        let mut c = LiveReplyController::with_options(io.clone(), options.clone());
        c.handle_event(&json!({"type":"assistant_thinking_delta","text":" detail "}))
            .await
            .unwrap();
        advance(99).await;
        assert!(io.activity().is_empty());
        advance(1).await;
        assert_eq!(
            io.activity()[0],
            Activity {
                kind: ActivityKind::Thinking,
                text: "正在思考…".into(),
                elapsed_ms: 100,
                update_count: 1,
                detail: Some("detail".into())
            }
        );
        advance(199).await;
        assert_eq!(io.activity().len(), 1);
        advance(1).await;
        assert_eq!(io.activity()[1].update_count, 2);
        advance(1000).await;
        assert_eq!(io.activity().len(), 2);
        let io = Fake::new(false, true);
        let mut c = LiveReplyController::with_options(
            io.clone(),
            LiveReplyOptions {
                activity_ttl_ms: 250,
                activity_max_updates: 6,
                ..options.clone()
            },
        );
        c.handle_event(&json!({"type":"turn_started"}))
            .await
            .unwrap();
        advance(100).await;
        advance(200).await;
        assert_eq!(io.activity().len(), 1);
        let io = Fake::new(false, true);
        io.pulses.lock().unwrap().push_back(Ok(Some(false)));
        let mut c = LiveReplyController::with_options(io.clone(), options);
        c.resume_activity(ActivityKind::Tool, true).await.unwrap();
        advance(1000).await;
        c.resume_activity(ActivityKind::Tool, true).await.unwrap();
        assert_eq!(io.activity().len(), 1);
    }

    // ImLiveReplyController.ts:174-204 activity events, :322 subagent transitions, :215 default (REPORT 120).
    #[tokio::test(start_paused = true)]
    async fn reply_activity_events_subagent_and_default_ignore() {
        let io = Fake::new(false, true);
        let mut c = LiveReplyController::new(io.clone());
        c.handle_event(&json!({"type":"model_request_started"}))
            .await
            .unwrap();
        advance(2500).await;
        c.handle_event(&json!({"type":"tool_call_started","name":"read_file"}))
            .await
            .unwrap();
        advance(2500).await;
        assert_eq!(io.activity()[1].kind, ActivityKind::Tool);
        assert_eq!(io.activity()[1].detail.as_deref(), Some("read_file"));
        c.handle_event(&json!({"type":"agent_status","event":"subagent_started","detail":{"subagentType":"worker"}})).await.unwrap();
        advance(2500).await;
        assert_eq!(io.activity()[2].kind, ActivityKind::Subagent);
        assert_eq!(io.activity()[2].detail.as_deref(), Some("worker"));
        c.handle_event(&json!({"type":"agent_status","event":"subagent_completed"}))
            .await
            .unwrap();
        advance(2500).await;
        assert_eq!(io.activity()[3].kind, ActivityKind::Thinking);
        let before = io.calls();
        c.handle_event(&json!({"type":"permission_request"}))
            .await
            .unwrap();
        c.handle_event(&json!({"type":"unknown"})).await.unwrap();
        assert_eq!(io.calls(), before);
    }

    // ImLiveReplyController.ts:188 tool boundary, :737 finalizeActivityOnly (REPORT 120,123).
    #[tokio::test(start_paused = true)]
    async fn reply_tool_boundary_activity_only_final() {
        let io = Fake::new(true, false);
        let mut c = LiveReplyController::new(io.clone());
        append(&mut c, "before tool").await;
        c.handle_event(&json!({"type":"tool_call_started","name":"tool"}))
            .await
            .unwrap();
        assert_eq!(io.messages(), vec![Call::Send("before tool".into())]);
        advance(2500).await;
        assert_eq!(io.messages()[1], Call::Send("正在执行工具… ▉".into()));
        c.flush_final().await.unwrap();
        assert_eq!(
            io.messages()[2],
            Call::Edit("处理完成，但没有可见回复。".into())
        );
        let io = Fake::new(false, true);
        let mut c = LiveReplyController::new(io.clone());
        c.resume_activity(ActivityKind::Thinking, true)
            .await
            .unwrap();
        c.flush_final().await.unwrap();
        assert_eq!(
            io.messages(),
            vec![Call::Send("处理完成，但没有可见回复。".into())]
        );
    }

    // ImLiveReplyController.ts:615 runExclusive, :607 waitForInFlight (REPORT 123).
    #[tokio::test(start_paused = true)]
    async fn reply_terminal_waits_for_in_flight_lock() {
        let io = Fake::new(false, false);
        let mut c = LiveReplyController::new(io.clone());
        let state = c.state.clone();
        let in_flight = state.lock().await;
        let operation = tokio::spawn(async move {
            c.mark_aborted().await.unwrap();
            c
        });
        tokio::task::yield_now().await;
        assert!(io.messages().is_empty());
        assert!(!operation.is_finished());
        drop(in_flight);
        let _controller = operation.await.unwrap();
        assert_eq!(
            io.messages(),
            vec![Call::Send("处理已中止，请重新发送或稍后重试。".into())]
        );
    }

    // ImLiveReplyController.ts:615 runExclusive with timer racing a slow send (REPORT 121,123).
    #[tokio::test(start_paused = true)]
    async fn reply_timeout_timer_serializes_with_slow_transport_send() {
        let io = Fake::new(true, false);
        let gate = Arc::new(tokio::sync::Notify::new());
        *io.first_send_gate.lock().unwrap() = Some(gate.clone());
        let mut c = LiveReplyController::with_options(
            io.clone(),
            LiveReplyOptions {
                turn_timeout_ms: 1000,
                ..Default::default()
            },
        );
        let task = tokio::spawn(async move {
            append(&mut c, &"a".repeat(24)).await;
            c
        });
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            io.messages(),
            vec![Call::Send(format!("{} ▉", "a".repeat(24)))]
        );
        advance(1000).await;
        assert_eq!(io.messages().len(), 1);
        assert!(!task.is_finished());
        gate.notify_one();
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        let mut c = task.await.unwrap();
        c.flush_final().await.unwrap();
        assert_eq!(
            io.messages(),
            vec![
                Call::Send(format!("{} ▉", "a".repeat(24))),
                Call::Edit("处理超时，请重新发送或稍后重试。".into())
            ]
        );
    }

    // ImLiveReplyController.ts:988 defaultErrorFormatter (REPORT 120).
    #[tokio::test(start_paused = true)]
    async fn reply_error_provider_details() {
        let io = Fake::new(false, false);
        let mut c = LiveReplyController::new(io.clone());
        c.handle_event(&json!({"type":"error","message":"bad","providerError":{"provider":"demo","status":429,"code":"busy","message":"retry","raw":"body"}})).await.unwrap();
        c.flush_final().await.unwrap();
        assert_eq!(io.messages(), vec![Call::Send("\n❌ bad\nProvider error:\n- provider: demo\n- status: 429\n- code: busy\n- message: retry\n- raw: body\n".into())]);
    }

    // weixin-render.ts:3 renderWeixinEvent six cases and default (REPORT 154; 八项④渲染).
    #[test]
    fn reply_render_weixin_event_six_classes() {
        assert_eq!(
            render_weixin_event(&json!({"type":"assistant_text_delta","text":" raw "})),
            Some(" raw ".into())
        );
        assert_eq!(
            render_weixin_event(&json!({"type":"assistant_thinking_delta","text":"secret"})),
            Some(String::new())
        );
        assert_eq!(
            render_weixin_event(&json!({"type":"tool_call_started"})),
            Some(String::new())
        );
        assert_eq!(
            render_weixin_event(&json!({"type":"tool_call_finished","ok":true})),
            Some(String::new())
        );
        assert_eq!(
            render_weixin_event(
                &json!({"type":"tool_call_finished","ok":false,"toolCallId":"tool-id"})
            ),
            Some("\n⚠️ tool-id 执行失败\n".into())
        );
        assert_eq!(
            render_weixin_event(
                &json!({"type":"tool_call_finished","ok":false,"toolName":"send_attachment","toolCallId":"id"})
            ),
            Some(String::new())
        );
        assert_eq!(
            render_weixin_event(
                &json!({"type":"tool_call_finished","ok":false,"toolCallId":"send_attachment"})
            ),
            Some(String::new())
        );
        assert_eq!(
            render_weixin_event(
                &json!({"type":"elicitation_request","questions":[{"header":"Title","question":"Choose","options":[{"label":"A"},{"label":"B"}]},{"question":"Next","options":[{"label":"C"}]}]})
            ),
            Some("\n**Title**\nChoose\n1. A\n2. B\nNext\n1. C\n".into())
        );
        assert_eq!(
            render_weixin_event(&json!({"type":"elicitation_request","questions":[]})),
            Some(String::new())
        );
        assert_eq!(
            render_weixin_event(&json!({"type":"error","message":"failure"})),
            Some("\n❌ failure\n".into())
        );
        assert_eq!(render_weixin_event(&json!({"type":"agent_status"})), None);
    }

    // WeixinChannel.ts:1069 formatWeixinActivity, :1081 formatElapsed (REPORT 119; 153 adjacent).
    #[test]
    fn reply_format_weixin_activity_elapsed_three_ranges() {
        assert_eq!(format_elapsed(59_999), "");
        assert_eq!(format_elapsed(60_000), "1 分钟");
        assert_eq!(format_elapsed(3_600_000), "1 小时");
        assert_eq!(format_elapsed(3_660_000), "1 小时 1 分钟");
        assert_eq!(
            format_weixin_activity(ActivityKind::Thinking, 0),
            "仍在处理：正在分析和生成回复"
        );
        assert_eq!(
            format_weixin_activity(ActivityKind::Tool, 120_000),
            "仍在处理：正在执行工具（已用时 2 分钟）"
        );
        assert_eq!(
            format_weixin_activity(ActivityKind::Subagent, 7_380_000),
            "仍在处理：正在处理子任务（已用时 2 小时 3 分钟）"
        );
    }

    struct FakeWeixinIo {
        calls: StdMutex<Vec<Call>>,
        fail: bool,
        client: bool,
        context: bool,
    }
    impl WeixinReplyIo for FakeWeixinIo {
        fn typing_available(&self) -> bool {
            self.client && self.context
        }
        fn send_reply<'a>(&'a self, text: &'a str, queue: bool) -> ChannelCall<'a, bool> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .unwrap()
                    .push(Call::Reply(text.into(), queue));
                if self.fail {
                    Err(failure("delivery failed"))
                } else {
                    Ok(false)
                }
            })
        }
        fn send_typing_if_possible(&self) -> ChannelCall<'_, ()> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(Call::Typing);
                Err(failure("typing failed"))
            })
        }
    }

    // WeixinChannel.ts:786 transport, :792 pulse, :797 stop, :880 typing (REPORT 115-118).
    #[tokio::test]
    async fn reply_weixin_transport_queue_and_typing_best_effort() {
        let io = Arc::new(FakeWeixinIo {
            calls: StdMutex::new(Vec::new()),
            fail: false,
            client: true,
            context: true,
        });
        let transport = WeixinLiveTransport::new(io.clone());
        assert!(!transport.has_edit());
        assert!(transport.has_pulse_activity());
        assert_eq!(transport.send("final").await.unwrap(), None);
        let activity = Activity {
            kind: ActivityKind::Tool,
            text: "activity".into(),
            elapsed_ms: 0,
            update_count: 1,
            detail: None,
        };
        assert_eq!(
            transport.pulse_activity(&activity).await.unwrap(),
            Some(true)
        );
        assert_eq!(transport.stop_activity().await.unwrap(), Some(true));
        transport.send_typing_if_possible().await;
        assert_eq!(
            *io.calls.lock().unwrap(),
            vec![
                Call::Reply("final".into(), true),
                Call::Reply("activity".into(), false),
                Call::Typing,
                Call::Typing
            ]
        );
        let io = Arc::new(FakeWeixinIo {
            calls: StdMutex::new(Vec::new()),
            fail: true,
            client: true,
            context: true,
        });
        let transport = WeixinLiveTransport::new(io.clone());
        assert_eq!(
            transport.pulse_activity(&activity).await.unwrap(),
            Some(true)
        );
        assert_eq!(
            *io.calls.lock().unwrap(),
            vec![Call::Reply("activity".into(), false), Call::Typing]
        );
        for (client, context) in [(false, true), (true, false)] {
            let io = Arc::new(FakeWeixinIo {
                calls: StdMutex::new(Vec::new()),
                fail: false,
                client,
                context,
            });
            let transport = WeixinLiveTransport::new(io.clone());
            transport.send_typing_if_possible().await;
            assert!(io.calls.lock().unwrap().is_empty());
            transport.pulse_activity(&activity).await.unwrap();
            assert_eq!(
                *io.calls.lock().unwrap(),
                vec![Call::Reply("activity".into(), false)]
            );
        }
    }
}
