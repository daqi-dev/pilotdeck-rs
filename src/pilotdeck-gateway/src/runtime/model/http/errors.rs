//! `normalizeModelError.ts`, `tokenLimitParsing.ts`, the retry-after parsers of
//! `protocol/errors.ts` and `normalizeNetworkError` of `network/fetch.ts`.
//!
//! The baseline regexes have no `u` flag and use `i`. ES `Canonicalize` never
//! maps a non-ASCII unit onto ASCII, so for these ASCII-literal patterns `i` is
//! exactly ASCII case folding. Patterns are written lower-case and matched
//! against an ASCII-lowercased copy (byte offsets unchanged); Rust's Unicode
//! `(?i)` would wrongly fold `ſ`/`K` onto `s`/`k`. `\d` is ASCII (`[0-9]`) and
//! `\s` is the ES whitespace set ([`crate::metadata::SPACE`]).
use crate::metadata::SPACE;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use regex::Regex;
use std::sync::OnceLock;

use super::{JsError, field, js_round};

macro_rules! pattern {
    ($name:ident, $source:expr) => {
        fn $name() -> &'static Regex {
            static CELL: OnceLock<Regex> = OnceLock::new();
            CELL.get_or_init(|| {
                Regex::new(&$source.replace("\\s", &format!("[{SPACE}]"))).expect("static regex")
            })
        }
    };
}

pattern!(
    retry_after_message,
    r"(?:try again|retry)\s+in\s+([0-9]+(?:\.[0-9]+)?)\s*(ms|milliseconds?|s|seconds?|minutes?|m)(?-u:\b)"
);
pattern!(prompt_too_long_anthropic, r"prompt is too long");
pattern!(
    prompt_too_long_openai,
    r"input length and max_tokens exceed context limit"
);
pattern!(request_too_large, r"request too large");
pattern!(
    max_output_reached,
    r"max(?:imum)? (?:output|completion) tokens? (?:exceeded|reached)"
);
pattern!(
    multimodal_processor,
    r"failed to apply.*processor|failed to load image|cannot identify image file|image decoding failed|invalid image|is not an? multimodal model|does not support (?:image|vision)(?: input)?|(?:image|vision) input (?:is )?not supported"
);
pattern!(
    context_overflow,
    r"context length|context size|maximum context|too many tokens|context window|prompt exceeds max length|maximum number of tokens|exceeds the max_model_len|max_model_len|input is too long|maximum model length|context length exceeded|slot context|n_ctx_slot|超过最大长度|上下文长度|input tokens? exceed|exceeds the maximum number of input tokens"
);
pattern!(
    billing,
    r"insufficient credits|insufficient_quota|insufficient balance|credit balance|credits have been exhausted|quota_exhausted|quota exhausted|quota has been exhausted|top up your credits|payment required|billing hard limit|exceeded your current quota|account is deactivated|plan does not include"
);
pattern!(
    invalid_api_key,
    r"invalid_api_key|invalid api key|incorrect api key|api key is invalid"
);
pattern!(
    model_not_found,
    r"is not a valid model|invalid model|model not found|model_not_found|does not exist|no such model|unknown model|unsupported model"
);
pattern!(
    image_too_large,
    r"image exceeds|image too large|image_too_large|image size exceeds"
);
pattern!(
    rate_limit_message,
    r"rate limit|rate_limit|too many requests|throttled|requests per minute|tokens per minute|resource_exhausted"
);
pattern!(
    transient_usage_signal,
    r"try again|retry|resets at|reset in|wait|requests remaining|window"
);
pattern!(
    usage_limit,
    r"usage limit|quota|limit exceeded|key limit exceeded"
);
pattern!(
    premature_stream_close,
    r"connection prematurely closed(?: during response)?|prematurely closed during response"
);
pattern!(
    network_timeout,
    r"fetch failed|terminated|socket hang up|etimedout|econnreset|econnrefused|network error|request timeout|stream idle timeout|no data received|client disconnected"
);
pattern!(stream_idle_hint, r"stream idle timeout|no data received");
pattern!(html_title, r"<title[^>]*>([^<]+)</title>");

fn lower(text: &str) -> String {
    text.to_ascii_lowercase()
}
fn test(pattern: fn() -> &'static Regex, text: &str) -> bool {
    pattern().is_match(&lower(text))
}

pub(crate) fn is_premature_stream_close(message: &str) -> bool {
    !message.is_empty() && test(premature_stream_close, message)
}

/// `parseRetryAfterFromMessage`.
pub(crate) fn retry_after_from_message(message: &str) -> Option<f64> {
    let lowered = lower(message);
    let captures = retry_after_message().captures(&lowered)?;
    let value: f64 = captures[1].parse().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let unit = &captures[2];
    if unit == "ms" || unit.starts_with("millisecond") {
        Some(js_round(value))
    } else if unit == "s" || unit.starts_with("second") {
        Some(js_round(value * 1000.0))
    } else if unit == "m" || unit.starts_with("minute") {
        Some(js_round(value * 60_000.0))
    } else {
        None
    }
}

/// `parseRetryAfterHeader` (protocol/errors.ts). Delta-seconds follow JS
/// `Number`; every other value goes through the full Node22 `Date.parse`
/// dialect set — IMF-fixdate, RFC 850, asctime, ISO and the V8-lenient forms
/// — shared with the execution clock (`pilotdeck_transport::system_clock`,
/// #360-2). A wall time without a timezone resolves against the process
/// timezone exactly like a Node `Date.parse`; where no admitted system TZif
/// can establish that offset — a state TS does not have — the value counts
/// as unparseable and the header is ignored.
pub(crate) fn retry_after_header(value: Option<&str>, now_ms: f64) -> Option<f64> {
    retry_after_header_parsed(
        value,
        now_ms,
        &pilotdeck_transport::system_clock::parse_node22_date,
    )
}

/// The injected-parse seam for timezone-dependent dialects: the production
/// entry resolves local wall times against the process timezone, so unit
/// fixtures pin the local resolver instead of the ambient one (the same
/// policy `parse_node22_date_with_local` itself exposes).
#[cfg(test)]
pub(super) fn retry_after_header_with_local<L>(
    value: Option<&str>,
    now_ms: f64,
    local: &L,
) -> Option<f64>
where
    L: Fn(i64) -> Result<i64, JsString> + ?Sized,
{
    retry_after_header_parsed(value, now_ms, &|text| {
        pilotdeck_transport::system_clock::parse_node22_date_with_local(text, &|wall| local(wall))
    })
}

fn retry_after_header_parsed(
    value: Option<&str>,
    now_ms: f64,
    parse: &dyn Fn(&JsString) -> Result<Option<f64>, JsString>,
) -> Option<f64> {
    let value = value.filter(|value| !value.is_empty())?;
    let trimmed = crate::metadata::js_trim(value);
    let seconds = crate::js_value::string_to_number(trimmed);
    if seconds.is_finite() && seconds >= 0.0 {
        return Some(js_round(seconds * 1000.0));
    }
    let delta = parse(&JsString::from(trimmed)).ok().flatten()? - now_ms;
    (delta > 0.0).then_some(delta)
}

/// `NetworkFetchError` of `network/fetch.ts`.
#[derive(Clone, Debug)]
pub struct NetworkFetchError {
    pub code: &'static str,
    pub message: JsString,
    /// JSON projection of `cause` (`undefined` when there was none).
    pub cause: JsValue,
}
impl NetworkFetchError {
    pub fn new(code: &'static str, message: impl Into<JsString>, cause: JsValue) -> Self {
        Self {
            code,
            message: message.into(),
            cause,
        }
    }
    /// `normalizeNetworkError` for a failure that is not an abort: classify by
    /// the cause's error code and the message.
    pub fn classify(message: &str, cause_code: Option<&str>) -> Self {
        let combined = lower(&format!("{} {message}", cause_code.unwrap_or("")));
        let has = |needle: &str| combined.contains(needle);
        let code = if has("enotfound") || has("eai_again") || has("dns") {
            "network_dns_error"
        } else if has("econnreset") || has("socket hang up") || has("terminated") {
            "network_connection_reset"
        } else if has("econnrefused") {
            "network_connection_refused"
        } else if has("etimedout") || has("timeout") {
            "network_timeout"
        } else if has("certificate") || has("tls") || has("ssl") {
            "network_tls_error"
        } else if has("proxy") {
            "network_proxy_error"
        } else if has("abort") {
            "network_abort"
        } else {
            "network_fetch_failed"
        };
        // undici's `TypeError: fetch failed` serializes as `{}`.
        Self::new(code, message, JsValue::Object(JsObject::new()))
    }
    pub(crate) fn as_js_error(&self) -> JsError {
        let mut raw = JsObject::new();
        raw.insert("code".into(), self.code.into());
        raw.insert("cause".into(), self.cause.clone());
        raw.insert("name".into(), "NetworkFetchError".into());
        JsError {
            message: self.message.clone(),
            code: Some(self.code.into()),
            network_code: Some(self.code),
            raw: JsValue::Object(raw),
        }
    }
}

/// What `normalizeModelError` receives: a parsed provider body or an Error.
pub(crate) enum ErrorInput<'a> {
    Value(&'a JsValue),
    Error(&'a JsError),
}

fn read_string(value: &JsValue) -> Option<&JsString> {
    value.as_string().filter(|s| !s.is_empty())
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

/// `normalizeModelError(provider, protocol, error, status)`.
pub(crate) fn normalize_model_error(
    provider: &JsString,
    protocol: &JsString,
    error: ErrorInput<'_>,
    status: Option<f64>,
) -> JsObject {
    static UNDEFINED: JsValue = JsValue::Undefined;
    // `firstErrorRecord`: a plain object, the first object element of an
    // array, or the Error instance itself (its own fields).
    let (record, raw, error_message, network_code): (
        Option<&JsValue>,
        JsValue,
        Option<&JsString>,
        Option<&str>,
    ) = match &error {
        ErrorInput::Value(value) => {
            let record = match value {
                JsValue::Object(_) => Some(*value),
                JsValue::Array(items) => items.iter().find(|i| matches!(i, JsValue::Object(_))),
                _ => None,
            };
            (record, (*value).clone(), None, None)
        }
        ErrorInput::Error(error) => (
            None,
            error.raw.clone(),
            Some(&error.message),
            error.network_code,
        ),
    };
    let nested = record
        .map(|r| field(r, "error"))
        .filter(|e| matches!(e, JsValue::Object(_)));
    let source = nested.or(record).unwrap_or(&UNDEFINED);
    let (source_message, source_code, source_type) = match &error {
        ErrorInput::Error(error) => (
            Some(error.message.clone()).filter(|m| !m.is_empty()),
            error.code.clone().filter(|c| !c.is_empty()),
            None,
        ),
        ErrorInput::Value(_) => (
            read_string(field(source, "message")).cloned(),
            read_string(field(source, "code")).cloned(),
            read_string(field(source, "type")).cloned(),
        ),
    };
    let raw_message: JsString = source_message
        .or_else(|| error_message.cloned())
        .unwrap_or_else(|| "Model provider request failed.".into());
    let raw_text = text(&raw_message);
    let message = sanitize_error_message(&raw_message);
    let message_text = text(&message);
    let code: JsString = classify_network_error(network_code, &message_text)
        .or_else(|| classify_semantic_error(&message_text, status))
        .map(JsString::from)
        .or(source_code)
        .or(source_type)
        .unwrap_or_else(|| status_code_to_code(status, &message_text).into());
    let code_text = text(&code);
    let mut result = JsObject::new();
    result.insert("provider".into(), JsValue::String(provider.clone()));
    result.insert("protocol".into(), JsValue::String(protocol.clone()));
    result.insert("code".into(), JsValue::String(code.clone()));
    result.insert(
        "status".into(),
        status.map_or(JsValue::Undefined, JsValue::Number),
    );
    result.insert("message".into(), JsValue::String(message.clone()));
    let retryable = match (&error, field(source, "retryable")) {
        (ErrorInput::Value(_), JsValue::Bool(b)) => *b,
        _ => is_retryable(status, &code_text),
    };
    result.insert("retryable".into(), JsValue::Bool(retryable));
    result.insert("raw".into(), raw);
    user_hint(&mut result, &code_text, &message_text, provider);
    if let ErrorInput::Value(_) = error
        && let JsValue::Number(ms) = field(source, "retryAfterMs")
        && ms.is_finite()
        && *ms >= 0.0
    {
        result.insert("retryAfterMs".into(), JsValue::Number(*ms));
    }
    let limits = parse_token_limit_error(&message_text);
    for (key, value) in [
        ("maxContextTokens", limits.max_context),
        ("maxOutputTokens", limits.max_output),
        ("availableOutputTokens", limits.available_output),
    ] {
        if let Some(value) = value {
            result.insert(key.into(), JsValue::Number(value));
        }
    }
    if code_text == "prompt_too_long" || code_text == "context_overflow" {
        result.insert("recoverableViaCompact".into(), JsValue::Bool(true));
    }
    if test(multimodal_processor, &message_text) || code_text == "image_too_large" {
        result.insert("recoverableViaImageStrip".into(), JsValue::Bool(true));
    }
    if let Some(ms) = retry_after_from_message(&raw_text) {
        result.insert("retryAfterMs".into(), JsValue::Number(ms));
    }
    result
}

fn classify_network_error(network_code: Option<&str>, message: &str) -> Option<&'static str> {
    if let Some(code) = network_code {
        return match code {
            "network_dns_error" => Some("dns_error"),
            "network_connection_reset" => Some("connection_reset"),
            "network_connection_refused" => Some("connection_refused"),
            "network_tls_error" => Some("tls_error"),
            "network_proxy_error" => Some("proxy_error"),
            "network_rate_limited" => Some("rate_limit_error"),
            "network_server_error" => Some("server_error"),
            "network_timeout" => Some("timeout"),
            _ => None,
        };
    }
    let text = lower(message);
    let has = |needle: &str| text.contains(needle);
    if is_premature_stream_close(message) {
        Some("connection_reset")
    } else if has("enotfound") || has("eai_again") || has("dns") {
        Some("dns_error")
    } else if has("econnreset") || has("socket hang up") {
        Some("connection_reset")
    } else if has("econnrefused") {
        Some("connection_refused")
    } else if has("certificate") || has("tls") || has("ssl") {
        Some("tls_error")
    } else if has("proxy connect")
        || has("proxy error")
        || has("tunnel")
        || has("econnrefused proxy")
    {
        Some("proxy_error")
    } else {
        None
    }
}

fn classify_semantic_error(message: &str, status: Option<f64>) -> Option<&'static str> {
    if test(invalid_api_key, message) {
        return Some("auth_error");
    }
    if test(prompt_too_long_anthropic, message) || test(prompt_too_long_openai, message) {
        return Some("prompt_too_long");
    }
    if test(request_too_large, message) {
        return Some("request_too_large");
    }
    if test(max_output_reached, message) {
        return Some("max_output_reached");
    }
    if status == Some(429.0)
        || test(rate_limit_message, message)
        || (test(usage_limit, message) && test(transient_usage_signal, message))
    {
        return Some("rate_limit_error");
    }
    if test(billing, message) {
        return Some("billing");
    }
    if test(image_too_large, message) {
        return Some("image_too_large");
    }
    if test(model_not_found, message) {
        return Some("model_not_found");
    }
    if test(context_overflow, message) {
        return Some("context_overflow");
    }
    if test(network_timeout, message) {
        return Some("timeout");
    }
    (status == Some(413.0)).then_some("payload_too_large")
}

fn is_retryable(status: Option<f64>, code: &str) -> bool {
    if [
        "auth_error",
        "billing",
        "model_not_found",
        "invalid_request",
    ]
    .contains(&code)
    {
        return false;
    }
    if matches!(status, Some(s) if s == 408.0 || s == 409.0 || s == 429.0 || s >= 500.0) {
        return true;
    }
    [
        "rate_limit_error",
        "overloaded_error",
        "timeout",
        "server_error",
        "dns_error",
        "connection_reset",
        "connection_refused",
        "proxy_error",
    ]
    .contains(&code)
}

fn status_code_to_code(status: Option<f64>, message: &str) -> &'static str {
    match status {
        Some(s) if s == 401.0 || s == 403.0 => "auth_error",
        Some(402.0) => {
            if test(billing, message) {
                "billing"
            } else if test(usage_limit, message) && test(transient_usage_signal, message) {
                "rate_limit_error"
            } else {
                "billing"
            }
        }
        Some(404.0) => "model_not_found",
        Some(413.0) => "payload_too_large",
        Some(429.0) => "rate_limit_error",
        Some(s) if s >= 500.0 => "server_error",
        _ => "provider_error",
    }
}

fn user_hint(result: &mut JsObject, code: &str, message: &str, provider: &JsString) {
    let provider_text = text(provider);
    let quoted = |prefix: &str| {
        if provider.is_empty() {
            String::new()
        } else {
            format!("{prefix} \"{provider_text}\"")
        }
    };
    let fix = |description: &str, key: &str, value: &str| {
        let mut fix = JsObject::new();
        fix.insert("description".into(), description.into());
        fix.insert(key.into(), value.into());
        JsValue::Object(fix)
    };
    let (hint, settings_fix): (String, Option<JsValue>) = match code {
        "billing" => (
            format!("API account balance exhausted or quota depleted{}. Top up provider billing or switch provider/model in Settings.", quoted(" for provider")),
            Some(fix("Top up credits or switch to a different provider.", "configPath", "model.provider")),
        ),
        "auth_error" => (
            format!(
                "API key rejected{}. Update the key in Settings → Model Provider or run pilotdeck setup.",
                if provider.is_empty() { " by the provider".to_string() } else { quoted(" by provider") }
            ),
            Some(fix("Reconfigure API key via setup.", "command", "pilotdeck setup")),
        ),
        "model_not_found" => (
            format!("The requested model does not exist or your account lacks access{}. Select a valid model in Settings → Model Provider or add it under model.providers.<id>.models in pilotdeck.yaml.", quoted(" on provider")),
            Some(fix("Switch to a valid model.", "configPath", "model.default")),
        ),
        "context_overflow" | "prompt_too_long" => (
            "Input exceeds the model context window. Run /compact, start a new session with /new, remove large attachments, or switch to a larger-context model in Settings.".into(),
            None,
        ),
        "image_too_large" => (
            "Image exceeds the provider per-image size limit (typically 5 MB). Resize and retry.".into(),
            None,
        ),
        "payload_too_large" | "request_too_large" => (
            "Request payload too large. Remove attachments, run /compact, start a new session with /new, or reduce the prompt size before retrying.".into(),
            None,
        ),
        "rate_limit_error" => (
            "Rate limited by the provider. Wait for the limit to reset, reduce concurrent requests, or switch provider/model in Settings.".into(),
            None,
        ),
        "overloaded_error" => (
            "Provider is temporarily overloaded. Retry later, check provider API status, or switch provider/model in Settings if it repeats.".into(),
            None,
        ),
        "max_output_reached" => (
            "Model output hit the token limit. Increase max output tokens in Settings → Model Provider or ask the agent to split the answer into smaller parts.".into(),
            None,
        ),
        "timeout" if test(stream_idle_hint, message) => (
            format!("Stream stalled{}. Increase retry.streamIdleTimeoutMs in Settings → Advanced, or check local network/proxy and provider status.", quoted(" for provider")),
            Some(fix("Increase stream idle timeout for this provider.", "configPath", "model.providers.<id>.retry.streamIdleTimeoutMs")),
        ),
        "timeout" => (
            format!("Request timed out{}. Increase provider.timeoutMs in Settings → Model Provider → Advanced, or check local network/proxy and provider status.", quoted(" for provider")),
            Some(fix("Increase request timeout for this provider.", "configPath", "model.providers.<id>.timeoutMs")),
        ),
        "server_error" => (
            "Provider returned a server error. Check provider API status/logs, retry later, or switch provider/model in Settings if it repeats.".into(),
            None,
        ),
        _ => return,
    };
    result.insert("userHint".into(), hint.as_str().into());
    if let Some(settings_fix) = settings_fix {
        result.insert("settingsFix".into(), settings_fix);
    }
}

/// One UTF-16 unit of the shared ES space set (all members are BMP).
fn is_js_space(unit: u16) -> bool {
    char::from_u32(u32::from(unit)).is_some_and(crate::metadata::js_space)
}
fn contains_units(haystack: &[u16], needle: &str) -> bool {
    let needle: Vec<u16> = needle.encode_utf16().collect();
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_slice())
}

/// `sanitizeErrorMessage`, over UTF-16 units like the baseline.
fn sanitize_error_message(raw: &JsString) -> JsString {
    let units = raw.units();
    if contains_units(units, "<!DOCTYPE") || contains_units(units, "<html") {
        let lowered = lower(&String::from_utf16_lossy(units));
        let original = String::from_utf16_lossy(units);
        if let Some(captures) = html_title().captures(&lowered) {
            let range = captures.get(1).expect("group").range();
            // ASCII lowering keeps byte offsets, so slice the original case.
            return crate::metadata::js_trim(&original[range]).into();
        }
        return "Service temporarily unavailable (HTML error page returned).".into();
    }
    let mut cleaned = Vec::with_capacity(units.len());
    let mut in_space = false;
    for &unit in units {
        if is_js_space(unit) {
            if !in_space {
                cleaned.push(0x20);
            }
            in_space = true;
        } else {
            cleaned.push(unit);
            in_space = false;
        }
    }
    let start = cleaned
        .iter()
        .position(|u| !is_js_space(*u))
        .unwrap_or(cleaned.len());
    let end = cleaned
        .iter()
        .rposition(|u| !is_js_space(*u))
        .map_or(start, |i| i + 1);
    let mut cleaned = cleaned[start..end].to_vec();
    if cleaned.len() > 300 {
        cleaned.truncate(297);
        cleaned.extend("...".encode_utf16());
    }
    JsString::from_utf16(cleaned)
}

#[derive(Default)]
struct TokenLimits {
    max_context: Option<f64>,
    max_output: Option<f64>,
    available_output: Option<f64>,
}

macro_rules! limit_pattern {
    ($name:ident, $source:expr) => {
        pattern!($name, $source);
    };
}
limit_pattern!(
    range_max_tokens,
    r"range of max_tokens should be\s*\[\s*[0-9]+\s*,\s*([0-9]+)\s*\]"
);
limit_pattern!(available_underscore, r"available_tokens[:\s]+([0-9]+)");
limit_pattern!(available_space, r"available\s+tokens[:\s]+([0-9]+)");
limit_pattern!(
    greater_than_context,
    r"max_tokens[:\s]+[0-9]+\s*>\s*(?:context_window[:\s]+)?([0-9]+)"
);
limit_pattern!(
    exceeds_context,
    r"max_tokens[:\s]+[0-9]+[^.]*?(?:exceeds|exceed|greater than)[^.]*?(?:context window|context_window|context length)[^0-9]*([0-9]+)"
);
limit_pattern!(
    at_most,
    r"max_(?:output_)?tokens?\s+(?:must be |should be |is )?(?:at most|<=|less than or equal to)\s*([0-9]+)"
);
limit_pattern!(
    completion_at_most,
    r"max_completion_tokens?\s+(?:must be |should be |is )?(?:at most|<=|less than or equal to)\s*([0-9]+)"
);
limit_pattern!(requested_output, r"requested\s+([0-9]+)\s+output tokens");
limit_pattern!(
    output_portion,
    r"maximum context length is\s*([0-9]+)(?s:.)*?([0-9]+)\s+(?:tokens?\s+)?(?:of|in)\s+(?:the\s+)?output"
);
limit_pattern!(maximum_context_is, r"maximum context length is\s*([0-9]+)");
limit_pattern!(
    prompt_contains,
    r"prompt (?:contains|has)\s+(?:at least\s+)?([0-9]+)\s+(?:tokens|characters)"
);
limit_pattern!(input_tokens, r"input_tokens?[:\s]+([0-9]+)");
limit_pattern!(ctx_limit, r"limit(?: is|:)?\s*([0-9]+)\s*tokens");
limit_pattern!(ctx_max_model_len, r"max_model_len[^0-9]{0,30}([0-9]+)");
limit_pattern!(ctx_maximum, r"maximum context length(?: is|:)?\s*([0-9]+)");
limit_pattern!(ctx_window, r"context[_\s-]?window(?: is|:)?\s*([0-9]+)");
limit_pattern!(
    ctx_length,
    r"context[_\s-]?length(?:_exceeded)?[^0-9]{0,40}([0-9]+)"
);
limit_pattern!(
    ctx_chinese,
    r"最多(?:支持|允许)?\s*([0-9]+)\s*(?:tokens?|个?token|上下文)"
);

fn capture(pattern: fn() -> &'static Regex, lowered: &str, group: usize) -> Option<String> {
    pattern().captures(lowered).map(|c| c[group].to_string())
}
fn positive(digits: Option<String>) -> Option<f64> {
    let value: f64 = digits?.parse().ok()?;
    (value.is_finite() && value > 0.0).then_some(value)
}

/// `parseTokenLimitError`.
fn parse_token_limit_error(message: &str) -> TokenLimits {
    let lowered = lower(message);
    let l = lowered.as_str();
    // Output limits first.
    if let Some(cap) = capture(range_max_tokens, l, 1) {
        let max_output = positive(Some(cap));
        if max_output.is_some() {
            return TokenLimits {
                max_output,
                ..Default::default()
            };
        }
        return context_limits(l);
    }
    let available = capture(available_underscore, l, 1).or_else(|| capture(available_space, l, 1));
    if let Some(available) = available.filter(|_| l.contains("max_tokens")) {
        let available_output = positive(Some(available));
        return if available_output.is_some() {
            TokenLimits {
                available_output,
                ..Default::default()
            }
        } else {
            context_limits(l)
        };
    }
    if let Some(cap) =
        capture(greater_than_context, l, 1).or_else(|| capture(exceeds_context, l, 1))
    {
        return output_or_context(positive(Some(cap)), None, l);
    }
    if let Some(cap) = capture(at_most, l, 1).or_else(|| capture(completion_at_most, l, 1)) {
        return output_or_context(positive(Some(cap)), None, l);
    }
    if capture(requested_output, l, 1).is_some() && l.contains("maximum context length") {
        return output_or_context(None, estimate_available_output(l), l);
    }
    if let Some(captures) = output_portion().captures(l) {
        let context = positive(Some(captures[1].to_string()));
        let used = positive(Some(captures[2].to_string()));
        if let (Some(context), Some(used)) = (context, used) {
            return TokenLimits {
                available_output: Some((context - used).max(1.0)),
                ..Default::default()
            };
        }
    }
    context_limits(l)
}
fn output_or_context(
    max_output: Option<f64>,
    available_output: Option<f64>,
    l: &str,
) -> TokenLimits {
    if max_output.is_some() || available_output.is_some() {
        TokenLimits {
            max_output,
            available_output,
            ..Default::default()
        }
    } else {
        context_limits(l)
    }
}
fn estimate_available_output(l: &str) -> Option<f64> {
    let context = positive(capture(maximum_context_is, l, 1))?;
    let prompt = capture(prompt_contains, l, 1).or_else(|| capture(input_tokens, l, 1));
    let prompt = prompt.and_then(|p| positive(Some(p)))?;
    Some((context - prompt).max(1.0))
}
fn context_limits(l: &str) -> TokenLimits {
    if l.contains("max_tokens")
        && (l.contains("available_tokens") || l.contains("range of max_tokens"))
    {
        return TokenLimits::default();
    }
    for pattern in [
        ctx_limit,
        ctx_max_model_len,
        ctx_maximum,
        ctx_window,
        ctx_length,
        ctx_chinese,
    ] {
        if let Some(value) = positive(capture(pattern, l, 1)) {
            return TokenLimits {
                max_context: Some(value),
                ..Default::default()
            };
        }
    }
    TokenLimits::default()
}

/// `error.retryAfterMs = headerMs` (an existing key keeps its position).
pub(crate) fn set_retry_after(error: &mut JsObject, ms: f64) {
    error.insert("retryAfterMs".into(), JsValue::Number(ms));
}
