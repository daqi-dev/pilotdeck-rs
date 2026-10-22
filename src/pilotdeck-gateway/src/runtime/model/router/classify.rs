//! `tokenSaver/classifyAndRoute.ts`: judge invocation, retry/timeout policy,
//! prompt construction and tier parsing. Provider HTTP is not performed here;
//! the judge arrives as a [`RouterModelInvocation`] port.
use super::{
    AbortFlag, RouterInvokeError, RouterModelInvocation, RouterModelRef, TierConfig,
    TokenSaverConfig, field, js_text, trim_js, trim_units,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use regex::Regex;
use std::sync::OnceLock;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct TokenSaverFailure {
    pub reason: &'static str,
    pub attempts: f64,
    pub code: Option<JsString>,
    pub message: Option<JsString>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TokenSaverDecision {
    pub tier: JsString,
    pub selection: Option<RouterModelRef>,
    pub resolved_from: &'static str,
    pub failure_reason: Option<&'static str>,
    pub failure: Option<TokenSaverFailure>,
}

const MAX_ATTEMPTS: usize = 3;

pub async fn classify_and_route(
    config: &TokenSaverConfig,
    messages: &[JsValue],
    invoker: &dyn RouterModelInvocation,
    abort: Option<AbortFlag>,
    previous_tier: Option<&JsString>,
    _session_id: &JsString,
) -> Result<Option<TokenSaverDecision>, RouterInvokeError> {
    if !config.enabled {
        return Ok(None);
    }
    let Some(default_tier) = config.tier(&config.default_tier) else {
        return Ok(None);
    };
    let Some(user_message) = extract_last_user_message(messages) else {
        return Ok(Some(TokenSaverDecision {
            tier: config.default_tier.clone(),
            selection: Some(default_tier.model.clone()),
            resolved_from: "default",
            failure_reason: None,
            failure: None,
        }));
    };

    let prompt = generate_judge_prompt(&user_message, config, previous_tier);
    let judge_request = judge_request(config, prompt);
    // JS Math.max propagates NaN; f64::max would silently replace it.
    let timeout_ms = if config.judge_timeout_ms.is_nan() {
        f64::NAN
    } else {
        config.judge_timeout_ms.max(500.0)
    };

    for attempt in 1..=MAX_ATTEMPTS {
        if attempt > 1 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if abort.as_ref().is_some_and(AbortFlag::is_aborted) {
            return Err(aborted_error());
        }
        // Each attempt gets its own child controller; the caller's signal is
        // only forwarded into it (TS `judgeAbortController`).
        let judge_abort = AbortFlag::new();
        let request_future = invoker.complete(judge_request.clone(), Some(judge_abort.clone()));
        let timed = tokio::time::timeout(
            crate::runtime::network::node_timer_duration(timeout_ms),
            request_future,
        );
        let outcome = match abort.as_ref() {
            Some(caller) => {
                tokio::select! {
                    biased;
                    _ = caller.cancelled() => {
                        judge_abort.abort();
                        return Err(aborted_error());
                    }
                    outcome = timed => outcome,
                }
            }
            None => timed.await,
        };

        match outcome {
            Err(_) => {
                // A judge timeout only aborts this attempt's child flag; the
                // caller's turn-level signal must stay untouched.
                judge_abort.abort();
                let failure = RouterInvokeError::other("Timeout");
                return Ok(Some(fallback(
                    config,
                    default_tier,
                    "timeout",
                    describe_failure(&failure, attempt as f64, true),
                )));
            }
            Ok(Ok(response)) => {
                let text = response_text(&response);
                if text.units().is_empty() {
                    if attempt < MAX_ATTEMPTS {
                        continue;
                    }
                    return Ok(Some(fallback(
                        config,
                        default_tier,
                        "parse_error",
                        TokenSaverFailure {
                            reason: "parse_error",
                            attempts: attempt as f64,
                            code: None,
                            message: None,
                        },
                    )));
                }
                let Some(tier) = parse_tier(&text, &config.tiers) else {
                    if attempt < MAX_ATTEMPTS {
                        continue;
                    }
                    return Ok(Some(fallback(
                        config,
                        default_tier,
                        "parse_error",
                        TokenSaverFailure {
                            reason: "parse_error",
                            attempts: attempt as f64,
                            code: None,
                            message: None,
                        },
                    )));
                };
                let Some(tier_config) = config.tier(&tier) else {
                    return Ok(Some(fallback(
                        config,
                        default_tier,
                        "parse_error",
                        TokenSaverFailure {
                            reason: "parse_error",
                            attempts: attempt as f64,
                            code: None,
                            message: None,
                        },
                    )));
                };
                return Ok(Some(TokenSaverDecision {
                    tier,
                    selection: Some(tier_config.model.clone()),
                    resolved_from: "judge",
                    failure_reason: None,
                    failure: None,
                }));
            }
            Ok(Err(error)) => {
                if abort.as_ref().is_some_and(AbortFlag::is_aborted) {
                    return Err(error);
                }
                if attempt < MAX_ATTEMPTS && should_retry_judge_failure(&error) {
                    continue;
                }
                return Ok(Some(fallback(
                    config,
                    default_tier,
                    "model_error",
                    describe_failure(&error, attempt as f64, false),
                )));
            }
        }
    }
    Ok(Some(fallback(
        config,
        default_tier,
        "parse_error",
        TokenSaverFailure {
            reason: "parse_error",
            attempts: MAX_ATTEMPTS as f64,
            code: None,
            message: None,
        },
    )))
}

fn fallback(
    config: &TokenSaverConfig,
    default_tier: &TierConfig,
    reason: &'static str,
    failure: TokenSaverFailure,
) -> TokenSaverDecision {
    TokenSaverDecision {
        tier: config.default_tier.clone(),
        selection: Some(default_tier.model.clone()),
        resolved_from: "fallback",
        failure_reason: Some(reason),
        failure: Some(failure),
    }
}

fn aborted_error() -> RouterInvokeError {
    RouterInvokeError::other("This operation was aborted")
}

fn describe_failure(
    error: &RouterInvokeError,
    attempts: f64,
    timed_out: bool,
) -> TokenSaverFailure {
    if timed_out {
        return TokenSaverFailure {
            reason: "timeout",
            attempts,
            code: Some(JsString::from("judge_timeout")),
            message: None,
        };
    }
    // TS only copies `code` off provider/request errors and only when truthy;
    // a plain `Error` never carries one (`From<ModuleError>` maps to `Other`).
    let code = match error.kind {
        super::InvokeErrorKind::Provider | super::InvokeErrorKind::Request => {
            error.code.clone().filter(|code| !code.units().is_empty())
        }
        super::InvokeErrorKind::Other => None,
    };
    // TS checks `message` for truthiness *before* sanitizing, so a message made
    // only of whitespace still produces an empty `errorMessage`.
    let message =
        (!error.message.units().is_empty()).then(|| sanitize_failure_message(&error.message));
    TokenSaverFailure {
        reason: "model_error",
        attempts,
        code,
        message,
    }
}

fn should_retry_judge_failure(error: &RouterInvokeError) -> bool {
    match error.kind {
        super::InvokeErrorKind::Request => false,
        super::InvokeErrorKind::Provider => error.retryable,
        super::InvokeErrorKind::Other => true,
    }
}

/// Port of TS `sanitizeFailureMessage` over UTF-16 code units.
///
/// JS regexes here have no `u` flag, so `\b`/`\w` are ASCII-only, `\s` is the
/// JS whitespace set (which includes U+FEFF and excludes U+0085), and `i`
/// canonicalizes one code unit at a time (non-ASCII characters are not folded
/// onto ASCII letters). Lone surrogates must survive the whole pipeline.
pub(super) fn sanitize_failure_message(message: &JsString) -> JsString {
    let redacted = replace_matches(
        &replace_matches(message.units(), "<redacted>", match_bearer),
        "=<redacted>",
        match_secret,
    );
    let mut collapsed = Vec::with_capacity(redacted.len());
    let mut index = 0;
    while index < redacted.len() {
        if super::is_js_whitespace(redacted[index]) {
            while index < redacted.len() && super::is_js_whitespace(redacted[index]) {
                index += 1;
            }
            collapsed.push(32);
        } else {
            collapsed.push(redacted[index]);
            index += 1;
        }
    }
    let trimmed = super::trim_units(&collapsed);
    JsString::from_utf16(trimmed.iter().take(300).copied().collect())
}

/// `(match_end, group1_start, group1_end)` for a replacement with one group.
type PatternMatch = (usize, usize, usize);

fn replace_matches(
    units: &[u16],
    replacement_suffix: &str,
    matcher: impl Fn(&[u16], usize) -> Option<PatternMatch>,
) -> Vec<u16> {
    let suffix: Vec<u16> = replacement_suffix.encode_utf16().collect();
    let mut output = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        if let Some((end, group_start, group_end)) = matcher(units, index) {
            output.extend_from_slice(&units[group_start..group_end]);
            output.extend_from_slice(&suffix);
            index = end;
        } else {
            output.push(units[index]);
            index += 1;
        }
    }
    output
}

/// `/\b(authorization\s*[:=]\s*bearer\s+|bearer\s+)[^\s,;]+/gi`
fn match_bearer(units: &[u16], start: usize) -> Option<PatternMatch> {
    if !is_word_boundary(units, start) {
        return None;
    }
    let mut after_keyword = None;
    if matches_ascii_ci(units, start, b"authorization") {
        let mut index = skip_js_space(units, start + "authorization".len());
        if matches!(units.get(index), Some(58 | 61)) {
            index = skip_js_space(units, index + 1);
            if matches_ascii_ci(units, index, b"bearer") {
                after_keyword = Some(index + "bearer".len());
            }
        }
    }
    let after_keyword =
        after_keyword.or_else(|| matches_ascii_ci(units, start, b"bearer").then_some(start + 6))?;
    let value_start = skip_js_space(units, after_keyword);
    if value_start == after_keyword {
        // Both alternatives require `\s+` before the value.
        return None;
    }
    let end = match_value(units, value_start)?;
    Some((end, start, value_start))
}

/// `/\b(api[_-]?key|token|secret|password)\s*[:=]\s*[^\s,;]+/gi`
fn match_secret(units: &[u16], start: usize) -> Option<PatternMatch> {
    if !is_word_boundary(units, start) {
        return None;
    }
    let mut keyword_end = None;
    if matches_ascii_ci(units, start, b"api") {
        let mut index = start + 3;
        if matches!(units.get(index), Some(45 | 95)) {
            index += 1;
        }
        if matches_ascii_ci(units, index, b"key") {
            keyword_end = Some(index + 3);
        }
    }
    if keyword_end.is_none() {
        for keyword in ["token", "secret", "password"] {
            if matches_ascii_ci(units, start, keyword.as_bytes()) {
                keyword_end = Some(start + keyword.len());
                break;
            }
        }
    }
    let keyword_end = keyword_end?;
    let mut index = skip_js_space(units, keyword_end);
    if !matches!(units.get(index), Some(58 | 61)) {
        return None;
    }
    index = skip_js_space(units, index + 1);
    let end = match_value(units, index)?;
    Some((end, start, keyword_end))
}

fn match_value(units: &[u16], start: usize) -> Option<usize> {
    let mut end = start;
    while end < units.len()
        && !super::is_js_whitespace(units[end])
        && units[end] != 44
        && units[end] != 59
    {
        end += 1;
    }
    (end > start).then_some(end)
}

fn skip_js_space(units: &[u16], mut index: usize) -> usize {
    while index < units.len() && super::is_js_whitespace(units[index]) {
        index += 1;
    }
    index
}

fn is_word_boundary(units: &[u16], at: usize) -> bool {
    let before = at > 0 && is_js_word_unit(units[at - 1]);
    let after = at < units.len() && is_js_word_unit(units[at]);
    before != after
}

pub(super) fn matches_ascii_ci(units: &[u16], at: usize, needle: &[u8]) -> bool {
    at + needle.len() <= units.len()
        && needle.iter().enumerate().all(|(offset, byte)| {
            canonicalize(units[at + offset]) == canonicalize(u16::from(*byte))
        })
}

/// ES non-unicode `i` equality for an arbitrary UTF-16 literal (config tier
/// names may contain non-ASCII units or lone surrogates).
pub(super) fn matches_units_ci(units: &[u16], at: usize, needle: &[u16]) -> bool {
    at + needle.len() <= units.len()
        && needle
            .iter()
            .enumerate()
            .all(|(offset, unit)| canonicalize(units[at + offset]) == canonicalize(*unit))
}

/// ES `Canonicalize` without the `u` flag: one code unit, at most one code
/// unit of uppercase, and non-ASCII characters never fold onto ASCII.
pub(super) fn canonicalize(unit: u16) -> u16 {
    let Some(character) = char::from_u32(u32::from(unit)) else {
        return unit; // lone surrogate
    };
    let mut upper = character.to_uppercase();
    let Some(mapped) = upper.next() else {
        return unit;
    };
    if upper.next().is_some() || mapped.len_utf16() != 1 {
        return unit;
    }
    let mapped = mapped as u32;
    if u32::from(unit) >= 128 && mapped < 128 {
        return unit;
    }
    mapped as u16
}

fn judge_request(config: &TokenSaverConfig, prompt: JsString) -> JsValue {
    let mut text = JsObject::new();
    text.insert("type".into(), "text".into());
    text.insert("text".into(), JsValue::String(prompt));
    let mut user = JsObject::new();
    user.insert("role".into(), "user".into());
    user.insert(
        "content".into(),
        JsValue::Array(vec![JsValue::Object(text)]),
    );
    let mut thinking = JsObject::new();
    thinking.insert("enabled".into(), JsValue::Bool(false));
    let mut request = JsObject::new();
    request.insert(
        "provider".into(),
        JsValue::String(config.judge.provider.clone()),
    );
    request.insert("model".into(), JsValue::String(config.judge.model.clone()));
    request.insert(
        "messages".into(),
        JsValue::Array(vec![JsValue::Object(user)]),
    );
    request.insert("maxOutputTokens".into(), JsValue::Number(256.0));
    request.insert("thinking".into(), JsValue::Object(thinking));
    request.insert("stream".into(), JsValue::Bool(false));
    JsValue::Object(request)
}

fn response_text(response: &JsValue) -> JsString {
    let JsValue::Array(blocks) = field(response, "content") else {
        return JsString::default();
    };
    let mut units = Vec::new();
    for block in blocks {
        if field(block, "type")
            .as_string()
            .is_some_and(|kind| kind.equals_str("text"))
            && let Some(value) = js_text(field(block, "text"))
        {
            units.extend_from_slice(value.units());
        }
    }
    JsString::from_utf16(units)
}

pub fn extract_last_user_message(messages: &[JsValue]) -> Option<JsString> {
    for message in messages.iter().rev() {
        if field(message, "role")
            .as_string()
            .is_none_or(|role| !role.equals_str("user"))
        {
            continue;
        }
        let metadata = field(message, "metadata");
        let synthetic = field(metadata, "synthetic").as_bool() == Some(true);
        let date_update = field(metadata, "purpose")
            .as_string()
            .is_some_and(|purpose| purpose.equals_str("date_update"));
        if synthetic && date_update {
            continue;
        }
        let JsValue::Array(blocks) = field(message, "content") else {
            continue;
        };
        let mut units = Vec::new();
        let mut first = true;
        for block in blocks {
            if field(block, "type")
                .as_string()
                .is_none_or(|kind| !kind.equals_str("text"))
            {
                continue;
            }
            let Some(text) = js_text(field(block, "text")) else {
                continue;
            };
            if !first {
                units.push(10);
            }
            first = false;
            units.extend_from_slice(text.units());
        }
        let trimmed = trim_units(&units);
        if !trimmed.is_empty() {
            return Some(JsString::from_utf16(trimmed.to_vec()));
        }
    }
    None
}

fn push_str(units: &mut Vec<u16>, text: &str) {
    units.extend_from_slice(&text.encode_utf16().collect::<Vec<_>>());
}

pub fn generate_judge_prompt(
    user_message: &JsString,
    config: &TokenSaverConfig,
    previous_tier: Option<&JsString>,
) -> JsString {
    let mut units = Vec::new();
    push_str(
        &mut units,
        "You are a model-tier classifier for the PilotDeck router. Given the following user message, return exactly one tier wrapped in <tier>...</tier>.\n\nAvailable tiers:\n",
    );
    for (index, (name, tier)) in config.tiers.iter().enumerate() {
        if index > 0 {
            units.push(10);
        }
        units.push(45);
        units.push(32);
        units.extend_from_slice(name.units());
        if let Some(description) = &tier.description
            && !description.units().is_empty()
        {
            push_str(&mut units, ": ");
            units.extend_from_slice(description.units());
        }
    }
    units.push(10);
    if !config.rules.is_empty() {
        push_str(&mut units, "\nRouting rules:\n");
        for (index, rule) in config.rules.iter().enumerate() {
            if index > 0 {
                units.push(10);
            }
            units.push(45);
            units.push(32);
            units.extend_from_slice(rule.units());
        }
        units.push(10);
    }
    if let Some(previous) = previous_tier
        && !previous.units().is_empty()
    {
        push_str(
            &mut units,
            "\n## CRITICAL RULE — Continuation messages\nThe previous turn was classified as: **",
        );
        units.extend_from_slice(previous.units());
        push_str(
            &mut units,
            "**.\nShort messages like \"go\", \"continue\", \"ok\", \"yes\", \"好的\", \"继续\", \"开始\", \"冲\" etc. are continuations of the previous task. They are NOT new simple requests.\nFor ANY message that is clearly a continuation or acknowledgment of the previous task, you MUST return <tier>",
        );
        units.extend_from_slice(previous.units());
        push_str(
            &mut units,
            "</tier>.\nOnly reclassify if the user message introduces a genuinely NEW task with different complexity.\n",
        );
    }
    push_str(&mut units, "\nUser message:\n\"\"\"\n");
    units.extend_from_slice(user_message.units());
    push_str(&mut units, "\n\"\"\"\n\nDefault tier when uncertain: ");
    units.extend_from_slice(config.default_tier.units());
    push_str(&mut units, ".\nRespond with only <tier>NAME</tier>.");
    JsString::from_utf16(units)
}

/// `/<tier>\s*([a-z0-9_-]+)\s*<\/tier>/i` over UTF-16 code units.
///
/// The TS regex has no `u` flag, so `i` is ES `Canonicalize` per code unit
/// rather than Unicode simple folding: U+017F/U+212A never match ASCII letters.
/// `\s` is the JS whitespace set from [`super::is_js_whitespace`].
fn find_tier_tag(units: &[u16]) -> Option<(usize, usize)> {
    for start in 0..units.len() {
        if units[start] != u16::from(b'<') || !matches_ascii_ci(units, start, b"<tier>") {
            continue;
        }
        let mut index = start + "<tier>".len();
        while index < units.len() && super::is_js_whitespace(units[index]) {
            index += 1;
        }
        let group_start = index;
        while index < units.len() && is_tier_tag_unit(units[index]) {
            index += 1;
        }
        if index == group_start {
            continue;
        }
        let group_end = index;
        while index < units.len() && super::is_js_whitespace(units[index]) {
            index += 1;
        }
        if matches_ascii_ci(units, index, b"</tier>") {
            return Some((group_start, group_end));
        }
    }
    None
}

/// `[a-z0-9_-]` under the non-unicode `i` flag.
fn is_tier_tag_unit(unit: u16) -> bool {
    matches!(canonicalize(unit), 48..=57 | 65..=90 | 95 | 45)
}

fn fence() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"```[a-z]*\n?").expect("fence"))
}
fn closing_fence() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"```").expect("closing fence"))
}

pub fn parse_tier(
    judge_output: &JsString,
    known_tiers: &[(JsString, TierConfig)],
) -> Option<JsString> {
    let text = String::from_utf16_lossy(judge_output.units());
    let cleaned = fence().replace_all(&text, "").into_owned();
    let cleaned = closing_fence().replace_all(&cleaned, "").into_owned();
    let cleaned = cleaned.trim();
    let cleaned_units: Vec<u16> = cleaned.encode_utf16().collect();

    if let Some((start, end)) = find_tier_tag(&cleaned_units) {
        let candidate = String::from_utf16_lossy(&cleaned_units[start..end]).to_ascii_lowercase();
        if let Some((name, _)) = known_tiers
            .iter()
            .find(|(name, _)| name.to_utf8().unwrap_or_default().to_lowercase() == candidate)
        {
            return Some(name.clone());
        }
    }
    for (name, _) in known_tiers {
        if contains_word(&cleaned_units, name.units()) {
            return Some(name.clone());
        }
    }
    None
}

/// JS `\b` is ASCII-word based (no `u` flag here), unlike Rust's Unicode `\b`;
/// the literal itself uses ES non-unicode `i` matching, so the haystack must not
/// be lowercased with Rust's full Unicode casing (U+212A would fold to `k`).
///
/// `\b` is a *transition* between a word and a non-word code unit, not merely
/// "both neighbours are non-word": at the start position it requires
/// `is_word(prev) != is_word(needle[0])`, and symmetrically at the end. For an
/// all-ASCII-word needle that is equivalent, but a needle starting or ending
/// with a non-word code unit (CJK, `-`, `.`, …) only matches when the adjacent
/// haystack code unit *is* a word unit — exactly what `new RegExp("\\b" + tier
/// + "\\b", "i")` does in the TS baseline.
fn contains_word(haystack: &[u16], needle: &[u16]) -> bool {
    let Some((&first, rest)) = needle.split_first() else {
        return false;
    };
    if needle.len() > haystack.len() {
        return false;
    }
    let last = *rest.last().unwrap_or(&first);
    let needle_starts_word = is_js_word_unit(first);
    let needle_ends_word = is_js_word_unit(last);
    for start in 0..=haystack.len() - needle.len() {
        if !matches_units_ci(haystack, start, needle) {
            continue;
        }
        let end = start + needle.len();
        let before_word = start > 0 && is_js_word_unit(haystack[start - 1]);
        let after_word = haystack.get(end).is_some_and(|unit| is_js_word_unit(*unit));
        if before_word != needle_starts_word && needle_ends_word != after_word {
            return true;
        }
    }
    false
}

fn is_js_word_unit(unit: u16) -> bool {
    matches!(unit, 48..=57 | 65..=90 | 97..=122 | 95)
}

pub fn is_short_continuation(message: &JsString) -> bool {
    let trimmed = trim_js(message);
    if trimmed.len_utf16() > 30 {
        return false;
    }
    is_continuation(trimmed.units())
}

/// `/^(go|ok|...|上)$/i` over UTF-16 code units (ES non-unicode casing).
fn is_continuation(units: &[u16]) -> bool {
    const CONTINUATIONS: &[&str] = &[
        "go",
        "ok",
        "yes",
        "y",
        "sure",
        "do it",
        "proceed",
        "continue",
        "next",
        "done",
        "start",
        "run",
        "好",
        "好的",
        "继续",
        "开始",
        "可以",
        "行",
        "嗯",
        "对",
        "是的",
        "没问题",
        "来吧",
        "冲",
        "走",
        "执行",
        "开搞",
        "干",
        "上",
    ];
    CONTINUATIONS
        .iter()
        .any(|pattern| matches_units_ci_str(units, pattern))
}

fn matches_units_ci_str(units: &[u16], pattern: &str) -> bool {
    let pattern: Vec<u16> = pattern.encode_utf16().collect();
    units.len() == pattern.len() && matches_units_ci(units, 0, &pattern)
}
