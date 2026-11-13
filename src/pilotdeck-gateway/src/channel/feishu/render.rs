//! The pieces of `ImLiveReplyController.ts` + `feishu-render.ts` (baseline
//! cc7c4e6) the feishu channel's outbound path uses: the final-message `post`
//! payload (`renderFeishuLivePost`), the transport chunking
//! (`splitForTransport`), and the event→text mapping the controller's silence
//! default formatters produce.
//!
//! The channel-level streaming machinery (activity pulses, cursor edits,
//! throttled segments, `flush/clear`) is out of scope for this unit and is
//! registered in `specs/gateway-parity/diffs/`; a turn therefore emits its
//! final text once instead of editing a live message.
use pilotdeck_protocol::{JsObject, JsString, JsValue};

/// `DEFAULT_ACTIVITY_ONLY_FINAL_TEXT`.
pub const DEFAULT_LIVE_CARD_FALLBACK_TEXT: &str = "处理完成，但没有可见回复。";
/// `MAX_TEXT_MESSAGE_LENGTH` in `FeishuChannel.ts`.
pub const MAX_TEXT_MESSAGE_LENGTH: usize = 4000;
/// `DEFAULT_CURSOR`.
pub const DEFAULT_LIVE_REPLY_CURSOR: &str = " ▉";
const DEFAULT_TIMEOUT_FINAL_TEXT: &str = "处理超时，请重新发送或稍后重试。";
const DEFAULT_ABORT_FINAL_TEXT: &str = "处理已中止，请重新发送或稍后重试。";

const DEFAULT_LIVE_CARD_MAX_TEXT_LENGTH: usize = 4000;
const TRUNCATION_SUFFIX: &str = "\n…（已截断）";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActivityKind {
    #[default]
    Thinking,
    Tool,
    Subagent,
}

fn activity_label(kind: ActivityKind) -> &'static str {
    match kind {
        ActivityKind::Tool => "正在执行工具…",
        ActivityKind::Subagent => "正在处理子任务…",
        ActivityKind::Thinking => "正在思考…",
    }
}

/// `text.replace(/\s*▉\s*$/u, "")`: trailing whitespace, one cursor glyph,
/// then the whitespace that preceded it. `\s` is ECMAScript whitespace, which
/// `metadata::js_trim_end` already implements.
fn strip_live_cursor(text: &str) -> String {
    let trimmed = crate::metadata::js_trim_end(text);
    match trimmed.strip_suffix('▉') {
        Some(head) => crate::metadata::js_trim_end(head).to_owned(),
        None => trimmed.to_owned(),
    }
}

/// True for the first unit of a surrogate pair: cutting *after* it would leave
/// a lone surrogate, which a Rust `String` cannot hold (registered diff
/// A8.3-CH-FEISHU-009).
fn is_high_surrogate(unit: u16) -> bool {
    (0xD800..=0xDBFF).contains(&unit)
}

/// JS `String.prototype.slice(0, n).trimEnd()` over UTF-16 units, except that
/// a cut landing between the two halves of a surrogate pair backs off one unit
/// (diff 009: TS keeps the lone surrogate, Rust would emit U+FFFD).
fn truncate_units(units: &[u16], keep: usize) -> String {
    let mut end = units.len().min(keep);
    if end > 0 && end < units.len() && is_high_surrogate(units[end - 1]) {
        end -= 1;
    }
    let head = &units[..end];
    crate::metadata::js_trim_end(&String::from_utf16_lossy(head)).to_owned()
}

fn normalize_text(
    text: &str,
    fallback: Option<&str>,
    max_len: usize,
    use_fallback: bool,
) -> String {
    let stripped = crate::metadata::js_trim(&strip_live_cursor(text)).to_owned();
    let visible = if !stripped.is_empty() {
        stripped
    } else if use_fallback {
        fallback
            .unwrap_or(DEFAULT_LIVE_CARD_FALLBACK_TEXT)
            .to_owned()
    } else {
        String::new()
    };
    let units: Vec<u16> = visible.encode_utf16().collect();
    if units.len() <= max_len {
        return visible;
    }
    format!(
        "{}{TRUNCATION_SUFFIX}",
        truncate_units(&units, max_len.saturating_sub(12))
    )
}

/// `renderFeishuLivePost`: the `post` message content wrapper.
pub fn render_live_post(
    text: &str,
    is_final: bool,
    activity_kind: Option<ActivityKind>,
) -> JsValue {
    // `stripped || (isFinal ? "处理完成，但没有可见回复。" : activityLabel(...))`
    let content = normalize_text(
        text,
        Some(DEFAULT_LIVE_CARD_FALLBACK_TEXT),
        DEFAULT_LIVE_CARD_MAX_TEXT_LENGTH,
        is_final,
    );
    let content = if content.is_empty() {
        activity_label(activity_kind.unwrap_or_default()).to_owned()
    } else {
        content
    };
    let mut text_node = JsObject::new();
    text_node.insert("tag".into(), JsValue::String("text".into()));
    text_node.insert("text".into(), JsValue::String(JsString::from(content)));
    let mut zh_cn = JsObject::new();
    zh_cn.insert(
        "content".into(),
        JsValue::Array(vec![JsValue::Array(vec![JsValue::Object(text_node)])]),
    );
    let mut post = JsObject::new();
    post.insert("zh_cn".into(), JsValue::Object(zh_cn));
    JsValue::Object(post)
}

/// `liveActivityKindFromText`.
pub fn activity_kind_from_text(text: &str) -> ActivityKind {
    if text.contains("正在执行工具") {
        ActivityKind::Tool
    } else if text.contains("正在处理子任务") {
        ActivityKind::Subagent
    } else {
        ActivityKind::Thinking
    }
}

/// `liveTextLimit()`: `maxMessageLength - cursor.length` (`Math.max(1, …)`),
/// the per-message unit budget the controller's `append` splits at.
pub fn live_text_limit(max: usize) -> usize {
    max.saturating_sub(DEFAULT_LIVE_REPLY_CURSOR.encode_utf16().count())
        .max(1)
}

/// The controller's `append` split: a hard cut at `limit` UTF-16 units per
/// message, with no separator search (that is the fallback-continuation
/// chunker's job, which this unit does not port).
pub fn live_chunks(text: &str, limit: usize) -> Vec<String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    if limit == 0 || units.len() <= limit {
        return vec![text.to_owned()];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < units.len() {
        let mut end = (start + limit).min(units.len());
        // Diff 009: TS's hard `slice` can leave a lone surrogate on either
        // side of the cut; a Rust `String` cannot, so the segment boundary
        // moves one unit out of the pair instead of emitting U+FFFD.
        if end < units.len() && is_high_surrogate(units[end - 1]) {
            if end - start > 1 {
                end -= 1;
            } else {
                end += 1;
            }
        }
        chunks.push(String::from_utf16_lossy(&units[start..end]));
        start = end;
    }
    chunks
}

/// `splitForTransport`: 4000-unit chunks, preferring the last newline and
/// then the last space in the second half, dropping the newlines that start a
/// continuation. Registered diff A8.3-CH-FEISHU-005: the transport splitter
/// exists but the current live-reply path chunks with `live_chunks` and never
/// calls this.
#[allow(dead_code)]
pub fn split_for_transport(text: &str, max: usize) -> Vec<String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    if max == 0 || units.len() <= max {
        return vec![text.to_owned()];
    }

    let mut chunks = Vec::new();
    let mut rest = units.as_slice();
    while rest.len() > max {
        let half = max / 2;
        // `lastIndexOf("\n", max)` includes index `max` itself.
        let window = &rest[..=max];
        let mut split = window
            .iter()
            .rposition(|&unit| unit == u16::from(b'\n'))
            .unwrap_or(usize::MAX);
        if split == usize::MAX || split < half {
            split = window
                .iter()
                .rposition(|&unit| unit == u16::from(b' '))
                .unwrap_or(usize::MAX);
        }
        if split == usize::MAX || split < half {
            split = max;
        }
        chunks.push(String::from_utf16_lossy(&rest[..split]));
        let mut tail = &rest[split..];
        while tail.first() == Some(&u16::from(b'\n')) {
            tail = &tail[1..];
        }
        rest = tail;
    }
    if !rest.is_empty() {
        chunks.push(String::from_utf16_lossy(rest));
    }
    chunks
}

fn text_of(value: Option<&JsValue>) -> Option<String> {
    value
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
}

fn js_truthy(value: Option<&JsValue>) -> bool {
    match value {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) | Some(JsValue::Bool(false)) => false,
        Some(JsValue::Bool(true)) | Some(JsValue::Object(_)) | Some(JsValue::Array(_)) => true,
        Some(JsValue::Number(number)) => *number != 0.0 && !number.is_nan(),
        Some(JsValue::String(text)) => !text.is_empty(),
    }
}

fn display(value: Option<&JsValue>) -> String {
    match value {
        None | Some(JsValue::Undefined) => "undefined".into(),
        Some(JsValue::Null) => "null".into(),
        Some(JsValue::Bool(value)) => value.to_string(),
        Some(JsValue::Number(value)) => crate::js_value::number_string(*value),
        Some(JsValue::String(value)) => String::from_utf16_lossy(value.units()),
        Some(JsValue::Array(_)) | Some(JsValue::Object(_)) => value
            .and_then(|value| crate::js_value::display(value).ok())
            .unwrap_or_else(|| "undefined".into()),
    }
}

/// `defaultErrorFormatter` (`\n❌ ${message}${providerLines}\n`).
pub fn format_error(event: &JsValue) -> String {
    let provider = event.get("providerError");
    let provider_lines = if js_truthy(provider) {
        let mut lines = vec![String::new(), "Provider error:".to_owned()];
        let provider = provider.expect("#1329 provider error");
        if let Some(value) = text_of(provider.get("provider")).filter(|value| !value.is_empty()) {
            lines.push(format!("- provider: {value}"));
        }
        if let Some(status) = provider.get("status").filter(|value| !value.is_undefined()) {
            lines.push(format!("- status: {}", display(Some(status))));
        }
        if let Some(code) = text_of(provider.get("code")).filter(|value| !value.is_empty()) {
            lines.push(format!("- code: {code}"));
        }
        if let Some(message) = text_of(provider.get("message")).filter(|value| !value.is_empty()) {
            lines.push(format!("- message: {message}"));
        }
        if let Some(raw) = text_of(provider.get("raw")).filter(|value| !value.is_empty()) {
            lines.push(format!("- raw: {raw}"));
        }
        lines.join("\n")
    } else {
        String::new()
    };
    format!("\n❌ {}{provider_lines}\n", display(event.get("message")))
}

/// `defaultToolErrorFormatter` (`\n⚠️ ${toolName ?? toolCallId} 执行失败\n`).
pub fn format_tool_error(event: &JsValue) -> String {
    let name = event
        .get("toolName")
        .filter(|value| !value.is_undefined() && !matches!(value, JsValue::Null))
        .or_else(|| event.get("toolCallId"));
    format!("\n⚠️ {} 执行失败\n", display(name))
}

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

/// `ImLiveReplyController.ts:102-106`: `WARNING_STATUS_EVENTS` is the
/// visible-failure list plus two entries, so the two can never drift
/// (`channel/weixin/live_reply.rs` keeps the same shape). Both memberships
/// come out of one lookup (#1725): `handle_agent_status` needs the
/// visible-failure list for the failure flag and the warning list for the
/// text append, and the shared list must not be scanned twice per event.
const WARNING_STATUS_EVENTS_EXTRA: &[&str] = &["structured_output_completed", "turn_aborted"];
fn status_event_memberships(name: &str) -> (bool, bool) {
    let visible_failure = VISIBLE_FAILURE_STATUS_EVENTS.contains(&name);
    let warning = visible_failure || WARNING_STATUS_EVENTS_EXTRA.contains(&name);
    (visible_failure, warning)
}

/// `isVisibleFailureStatusDetail`: `detail.visible !== false && severity === "error"`.
fn is_visible_failure_detail(detail: Option<&JsValue>) -> bool {
    let Some(detail) = detail.and_then(JsValue::as_object) else {
        return false;
    };
    let visible = detail.get("visible");
    let visible_ok = !matches!(visible, Some(JsValue::Bool(false)));
    visible_ok && text_of(detail.get("severity")).as_deref() == Some("error")
}

/// The text the channel's live-reply path appends for one gateway event, with
/// the `ImLiveReplyController` default formatters. `None` means the event
/// only drives activity state, which this unit does not render.
#[derive(Default)]
pub struct LiveReplyBuffer {
    pub text: String,
    /// `closed`: a terminal event (`agent_aborted` / `turn_timeout`) already
    /// finalized the message, so later events are ignored.
    pub closed: bool,
    has_visible_failure_status: bool,
}

impl LiveReplyBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// `handleEvent(event)`; appends the formatted fragment when there is one.
    pub fn handle_event(&mut self, event: &JsValue) {
        if self.closed {
            return;
        }
        let Some(kind) = text_of(event.get("type")) else {
            return;
        };
        match kind.as_str() {
            "turn_started" => self.has_visible_failure_status = false,
            "assistant_text_delta" => {
                if let Some(text) = text_of(event.get("text")) {
                    self.text.push_str(&text);
                }
            }
            "tool_call_finished" => {
                if !js_truthy(event.get("ok")) {
                    self.text.push_str(&format_tool_error(event));
                }
            }
            "agent_status" => self.handle_agent_status(event),
            "error" => {
                // `markAborted` / `markTimedOut` replace the whole message
                // with the terminal text and close the controller.
                match text_of(event.get("code")).as_deref() {
                    Some("agent_aborted") => {
                        self.text = DEFAULT_ABORT_FINAL_TEXT.to_owned();
                        self.closed = true;
                    }
                    Some("turn_timeout") => {
                        self.text = DEFAULT_TIMEOUT_FINAL_TEXT.to_owned();
                        self.closed = true;
                    }
                    _ => {
                        if !self.has_visible_failure_status {
                            self.text.push_str(&format_error(event));
                        }
                    }
                }
            }
            // Activity-only events (`model_request_started`,
            // `assistant_thinking_delta`, `tool_call_started`,
            // `elicitation_request`), plus `assistant_attachment`, which the
            // attachment diff covers.
            _ => {}
        }
    }

    /// The submit-failure status the channel synthesizes on a gateway error:
    /// `channel_submit_failed` is a warning status, so the controller appends
    /// its message.
    pub fn handle_submit_failure(&mut self, message: &str) {
        self.has_visible_failure_status = true;
        self.text.push_str(&format!("\n⚠️ {message}\n"));
    }

    fn handle_agent_status(&mut self, event: &JsValue) {
        let name = text_of(event.get("event")).unwrap_or_default();
        let detail = event.get("detail");
        let (visible_failure_status, warning_status) = status_event_memberships(name.as_str());
        let visible_failure = detail
            .and_then(JsValue::as_object)
            .map(|detail| !matches!(detail.get("visible"), Some(JsValue::Bool(false))))
            .unwrap_or(true)
            && (visible_failure_status || is_visible_failure_detail(detail));
        if visible_failure {
            self.has_visible_failure_status = true;
        }
        let detail_message = detail
            .and_then(JsValue::as_object)
            .and_then(|detail| text_of(detail.get("message")));
        match name.as_str() {
            "model_empty_response_exhausted" => self.text.push_str(&format!(
                "\n⚠️ {}\n",
                detail_message.unwrap_or_else(|| {
                    "The model returned empty content repeatedly, so this turn has stopped. Try \
                     again later or increase max output tokens."
                        .to_owned()
                })
            )),
            "max_turns_reached" => self.text.push_str(&format!(
                "\n⚠️ {}\n",
                detail_message.unwrap_or_else(|| {
                    "Reached the maximum number of turns, so this turn has stopped. Increase \
                     maxTurns or split the task into smaller steps and try again."
                        .to_owned()
                })
            )),
            _ => {
                if (warning_status || is_visible_failure_detail(detail))
                    && let Some(message) = detail_message
                {
                    self.text.push_str(&format!("\n⚠️ {message}\n"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_json(raw: &str) -> JsValue {
        pilotdeck_protocol::parse_json(raw).expect("#1329 event json")
    }

    #[test]
    fn final_post_uses_the_fallback_for_empty_text() {
        let post = render_live_post(" ", true, None);
        assert_eq!(post_text(&post), DEFAULT_LIVE_CARD_FALLBACK_TEXT);
    }

    #[test]
    fn live_post_strips_the_cursor_and_labels_empty() {
        let post = render_live_post("working ▉", false, Some(ActivityKind::Tool));
        assert_eq!(post_text(&post), "working");
        let empty = render_live_post("", false, Some(ActivityKind::Thinking));
        assert_eq!(post_text(&empty), "正在思考…");
    }

    #[test]
    fn events_append_like_the_controller_default_formatters() {
        let mut buffer = LiveReplyBuffer::new();
        buffer.handle_event(&parse_json(
            r#"{"type":"assistant_text_delta","text":"hi"}"#,
        ));
        buffer.handle_event(&parse_json(
            r#"{"type":"assistant_thinking_delta","text":"x"}"#,
        ));
        buffer.handle_event(&parse_json(r#"{"type":"tool_call_finished","ok":true}"#));
        assert_eq!(buffer.text, "hi");

        buffer.handle_event(&parse_json(
            r#"{"type":"tool_call_finished","ok":false,"toolName":"bash","resultPreview":"boom"}"#,
        ));
        assert_eq!(buffer.text, "hi\n⚠️ bash 执行失败\n");

        buffer.handle_event(&parse_json(r#"{"type":"error","message":"boom"}"#));
        assert_eq!(buffer.text, "hi\n⚠️ bash 执行失败\n\n❌ boom\n");
    }

    #[test]
    fn visible_failure_status_suppresses_later_errors() {
        let mut buffer = LiveReplyBuffer::new();
        buffer.handle_event(&parse_json(
            r#"{"type":"agent_status","event":"turn_failed","detail":{"message":"bad"}}"#,
        ));
        buffer.handle_event(&parse_json(r#"{"type":"error","message":"secondary"}"#));
        assert_eq!(buffer.text, "\n⚠️ bad\n");
    }

    #[test]
    fn live_chunks_never_split_a_surrogate_pair() {
        let text = format!("{}😀{}", "a".repeat(3997), "b".repeat(10));
        let chunks = live_chunks(&text, 3998);
        crate::negative_assert!(
            chunks.iter().all(|chunk| !chunk.contains('\u{FFFD}')),
            "#1329 no chunk carries a replacement character"
        );
        crate::negative_assert_eq!(chunks.concat(), text, "#1329 chunks rejoin the reply");
        crate::negative_assert_eq!(chunks.len(), 2, "#1329 the boundary moved one unit out");
        // The same guard in the post truncation: the keep window ends right
        // after the high surrogate, so the cut backs off instead of emitting
        // U+FFFD (keep is TS's `maxTextLength - 12` head budget, i.e. 3988).
        let long = format!("{}😀{}", "a".repeat(3987), "b".repeat(40));
        let truncated = post_text(&render_live_post(&long, true, None));
        crate::negative_assert!(
            !truncated.contains('\u{FFFD}'),
            "#1329 the truncated post carries no replacement character"
        );
        crate::negative_assert!(
            truncated.ends_with("…（已截断）"),
            "#1329 the oversized post still reports truncation"
        );
    }

    #[test]
    fn split_prefers_newline_then_space_then_hard_cut() {
        let text = format!("{}\n{}", "a".repeat(30), "b".repeat(30));
        assert_eq!(
            split_for_transport(&text, 40),
            vec!["a".repeat(30), "b".repeat(30)]
        );
        let hard = "x".repeat(90);
        assert_eq!(split_for_transport(&hard, 40).len(), 3);
        assert_eq!(split_for_transport("short", 40), vec!["short".to_owned()]);
    }

    fn post_text(post: &JsValue) -> String {
        post.get("zh_cn")
            .and_then(|value| value.get("content"))
            .and_then(|value| match value {
                JsValue::Array(rows) => rows.first(),
                _ => None,
            })
            .and_then(|row| match row {
                JsValue::Array(nodes) => nodes.first(),
                _ => None,
            })
            .and_then(|node| node.get("text"))
            .map(|text| display(Some(text)))
            .expect("#1329 post text")
    }
}
