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
    let timeout_ms = config.judge_timeout_ms.max(500.0);

    for attempt in 1..=MAX_ATTEMPTS {
        if attempt > 1 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if abort.as_ref().is_some_and(AbortFlag::is_aborted) {
            return Err(RouterInvokeError::other("This operation was aborted"));
        }
        let judge_abort = abort.clone().unwrap_or_default();
        let outcome = tokio::time::timeout(
            Duration::from_millis(timeout_ms as u64),
            invoker.complete(judge_request.clone(), Some(judge_abort.clone())),
        )
        .await;

        match outcome {
            Err(_) => {
                // TS aborts the judge controller with a timeout error before
                // falling back; mirror the observable cancellation.
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
    TokenSaverFailure {
        reason: "model_error",
        attempts,
        code: error.code.clone(),
        message: Some(sanitize_failure_message(&error.message)),
    }
}

fn should_retry_judge_failure(error: &RouterInvokeError) -> bool {
    match error.kind {
        super::InvokeErrorKind::Request => false,
        super::InvokeErrorKind::Provider => error.retryable,
        super::InvokeErrorKind::Other => true,
    }
}

fn sanitize_failure_message(message: &JsString) -> JsString {
    let text = String::from_utf16_lossy(message.units());
    let text = bearer().replace_all(&text, "$1<redacted>").into_owned();
    let text = secret().replace_all(&text, "$1=<redacted>").into_owned();
    let trimmed = whitespace().replace_all(text.trim(), " ").into_owned();
    let units: Vec<u16> = trimmed.encode_utf16().take(300).collect();
    JsString::from_utf16(units)
}
fn bearer() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)\b(authorization\s*[:=]\s*bearer\s+|bearer\s+)[^\s,;]+")
            .expect("bearer redaction")
    })
}
fn secret() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)\b(api[_-]?key|token|secret|password)\s*[:=]\s*[^\s,;]+")
            .expect("secret redaction")
    })
}
fn whitespace() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\s+").expect("whitespace"))
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
        if let Some(description) = &tier.description {
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
    if let Some(previous) = previous_tier {
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

fn tier_tag() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"(?i)<tier>\s*([a-z0-9_-]+)\s*</tier>").expect("tier tag"))
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

    if let Some(captures) = tier_tag().captures(cleaned)
        && let Some(candidate) = captures.get(1)
    {
        let candidate = candidate.as_str().to_lowercase();
        if let Some((name, _)) = known_tiers
            .iter()
            .find(|(name, _)| name.to_utf8().unwrap_or_default().to_lowercase() == candidate)
        {
            return Some(name.clone());
        }
    }
    for (name, _) in known_tiers {
        if contains_word(cleaned, &name.to_utf8().unwrap_or_default()) {
            return Some(name.clone());
        }
    }
    None
}

/// JS `\b` is ASCII-word based (no `u` flag here), unlike Rust's Unicode `\b`.
fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let lower_haystack = haystack.to_lowercase();
    let lower_needle = needle.to_lowercase();
    let mut from = 0;
    while let Some(offset) = lower_haystack[from..].find(&lower_needle) {
        let start = from + offset;
        let end = start + lower_needle.len();
        let before_ok = lower_haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|ch| !is_js_word(ch));
        let after_ok = lower_haystack[end..]
            .chars()
            .next()
            .is_none_or(|ch| !is_js_word(ch));
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}
fn is_js_word(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

pub fn is_short_continuation(message: &JsString) -> bool {
    let trimmed = trim_js(message);
    if trimmed.len_utf16() > 30 {
        return false;
    }
    continuation().is_match(&String::from_utf16_lossy(trimmed.units()))
}
fn continuation() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)^(go|ok|yes|y|sure|do it|proceed|continue|next|done|start|run|好|好的|继续|开始|可以|行|嗯|对|是的|没问题|来吧|冲|走|执行|开搞|干|上)$")
            .expect("continuation")
    })
}
