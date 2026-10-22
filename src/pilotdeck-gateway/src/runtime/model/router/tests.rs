//! Unit coverage for router pieces that the real-TS fixture does not pin
//! (parser edges, retry admission, TTL/LRU, materialization).
use super::classify::{classify_and_route, extract_last_user_message, parse_tier};
use super::*;
use crate::runtime::model::fallback::RouterStreamFuture;
use crate::runtime::model::http::{StreamOptions, StreamOutput};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

fn text(value: &str) -> JsValue {
    JsValue::String(JsString::from(value))
}
fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn user_message(value: &str) -> JsValue {
    object(vec![
        ("role", text("user")),
        (
            "content",
            JsValue::Array(vec![object(vec![
                ("type", text("text")),
                ("text", text(value)),
            ])]),
        ),
    ])
}

fn saver() -> TokenSaverConfig {
    TokenSaverConfig {
        enabled: true,
        judge: RouterModelRef::new("p".into(), "j".into()),
        default_tier: "simple".into(),
        tiers: vec![
            (
                "simple".into(),
                TierConfig {
                    model: RouterModelRef::new("p".into(), "s".into()),
                    description: Some("Simple".into()),
                },
            ),
            (
                "complex".into(),
                TierConfig {
                    model: RouterModelRef::new("p".into(), "c".into()),
                    description: Some("Complex".into()),
                },
            ),
        ],
        rules: Vec::new(),
        subagent_policy: SubagentPolicy::Judge,
        judge_timeout_ms: 5_000.0,
        cache_aware: CacheAwareSwitching {
            enabled: true,
            min_savings_ratio: 0.0,
        },
    }
}

#[derive(Clone)]
enum Step {
    Text(String),
    Empty,
    Hang,
    Provider(bool),
}

struct Invoker {
    steps: Mutex<VecDeque<Step>>,
    calls: AtomicUsize,
    last_abort: Mutex<Option<AbortFlag>>,
    capabilities: Mutex<HashMap<String, f64>>,
    multimodal: Mutex<HashMap<String, Vec<JsString>>>,
}
impl Invoker {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
            calls: AtomicUsize::new(0),
            last_abort: Mutex::new(None),
            capabilities: Mutex::new(HashMap::from([
                ("p/s".to_string(), 100.0),
                ("p/c".to_string(), 300.0),
                ("p/j".to_string(), 4096.0),
            ])),
            multimodal: Mutex::new(HashMap::new()),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    fn last_abort(&self) -> Option<AbortFlag> {
        self.last_abort.lock().expect("last abort").clone()
    }
}
impl RouterModelInvocation for Invoker {
    fn get_capabilities(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, crate::host::ModuleError> {
        let key = format!(
            "{}/{}",
            provider.to_utf8().unwrap_or_default(),
            model.to_utf8().unwrap_or_default()
        );
        let caps = self.capabilities.lock().expect("capabilities");
        let Some(max) = caps.get(&key) else {
            return Err(crate::host::ModuleError::new("unknown model"));
        };
        Ok(object(vec![("maxOutputTokens", JsValue::Number(*max))]))
    }
    fn get_multimodal(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, crate::host::ModuleError> {
        let key = format!(
            "{}/{}",
            provider.to_utf8().unwrap_or_default(),
            model.to_utf8().unwrap_or_default()
        );
        let modalities = self.multimodal.lock().expect("multimodal");
        let input = modalities
            .get(&key)
            .cloned()
            .unwrap_or_else(|| vec!["text".into()]);
        Ok(object(vec![(
            "input",
            JsValue::Array(input.into_iter().map(JsValue::String).collect()),
        )]))
    }
    fn supports_complete(&self) -> bool {
        true
    }
    fn complete(&self, _request: JsValue, abort: Option<AbortFlag>) -> RouterFuture<JsValue> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_abort.lock().expect("last abort") = abort;
        let step = self
            .steps
            .lock()
            .expect("steps")
            .pop_front()
            .unwrap_or(Step::Text("<tier>simple</tier>".into()));
        match step {
            Step::Text(value) => Box::pin(async move {
                Ok(object(vec![(
                    "content",
                    JsValue::Array(vec![object(vec![
                        ("type", text("text")),
                        ("text", JsValue::String(value.into())),
                    ])]),
                )]))
            }),
            Step::Empty => {
                Box::pin(async move { Ok(object(vec![("content", JsValue::Array(Vec::new()))])) })
            }
            Step::Hang => Box::pin(async {
                std::future::pending::<Result<JsValue, RouterInvokeError>>().await
            }),
            Step::Provider(retryable) => Box::pin(async move {
                Err(RouterInvokeError::provider(
                    "auth_error",
                    "Authorization: Bearer sk-secret",
                    retryable,
                ))
            }),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn classify_non_retryable_provider_error_falls_back_immediately() {
    let invoker = Invoker::new(vec![
        Step::Provider(false),
        Step::Text("<tier>complex</tier>".into()),
    ]);
    let decision = classify_and_route(
        &saver(),
        &[user_message("hi")],
        &invoker,
        None,
        None,
        &"s".into(),
    )
    .await
    .expect("no abort")
    .expect("decision");
    assert_eq!(decision.resolved_from, "fallback");
    assert_eq!(
        invoker.calls(),
        1,
        "non-retryable provider errors are not retried"
    );
}

#[test]
fn strip_subagent_tag_preserves_leading_whitespace() {
    let messages = vec![user_message(
        "  <pilotdeck-subagent-model>p/m</pilotdeck-subagent-model>  tail  ",
    )];
    let stripped = scenario::strip_subagent_tag_from_messages(&messages);
    let blocks = stripped[0]
        .get("content")
        .and_then(|value| match value {
            JsValue::Array(items) => Some(items),
            _ => None,
        })
        .expect("content");
    let text = blocks[0]
        .get("text")
        .and_then(JsValue::as_string)
        .map(|value| value.to_utf8().unwrap());
    // `.replace(...).trimEnd()` leaves the tag's surrounding leading spaces.
    assert_eq!(text.as_deref(), Some("    tail"));
}

#[test]
fn parse_tier_matches_ts_word_boundaries() {
    let config = saver();
    let parse = |value: &str| parse_tier(&JsString::from(value), &config.tiers);
    assert!(parse("<tier>complex</tier>").is_some_and(|tier| tier.equals_str("complex")));
    assert!(parse("```ts\n<tier>SIMPLE</tier>\n```").is_some_and(|tier| tier.equals_str("simple")));
    assert!(parse("I choose simple.").is_some_and(|tier| tier.equals_str("simple")));
    assert!(parse("复杂 simple 复杂").is_some_and(|tier| tier.equals_str("simple")));
    // JS `\b` without the `u` flag: ASCII word characters only.
    assert!(parse("xsimple").is_none());
    assert!(parse("simpleton").is_none());
    assert!(parse("nothing to see").is_none());
}

/// Goldens below are the literal output of the real TS `parseTier` at
/// `PilotDeck@cc7c4e65` for the same tier list (node probe
/// `_tmp/20261019-a64-pr4/probe-tier-boundary.json`), covering needles whose
/// first/last code unit is not an ASCII word unit. `use 简单 mode` and
/// `x简单y` are mutual negative controls: the old "both neighbours non-word"
/// check accepted the first and rejected the second, JS does the opposite.
#[test]
fn parse_tier_matches_js_word_boundary_goldens() {
    let tiers: Vec<(JsString, TierConfig)> = ["简单", "beta-", "τ", "simple", "fallback"]
        .into_iter()
        .map(|name| {
            (
                JsString::from(name),
                TierConfig {
                    model: RouterModelRef::new("p".into(), "m".into()),
                    description: None,
                },
            )
        })
        .collect();
    let cases = [
        ("use 简单 mode", None),
        ("简单", None),
        ("x简单y", Some("简单")),
        ("use beta- now", None),
        ("beta-", None),
        ("beta-x", Some("beta-")),
        ("xbeta-y", None),
        ("(beta-)", None),
        ("use τ now", None),
        ("τ", None),
        ("xτy", Some("τ")),
        ("use simple now", Some("simple")),
        ("xsimple", None),
        ("simpleton", None),
        ("use fallback now", Some("fallback")),
        ("fallbackx", None),
    ];
    for (text, expected) in cases {
        let parsed = parse_tier(&JsString::from(text), &tiers)
            .map(|tier| tier.to_utf8().expect("utf-8 tier"));
        assert_eq!(parsed.as_deref(), expected, "parseTier({text:?})");
    }
}

#[test]
fn tier_tag_and_fallback_follow_js_non_unicode_case_folding() {
    let config = saver();
    let mut tiers = config.tiers.clone();
    tiers.push((
        "k".into(),
        TierConfig {
            model: RouterModelRef::new("p".into(), "k".into()),
            description: None,
        },
    ));
    let parse = |value: &str| parse_tier(&JsString::from(value), &tiers);
    // U+212A KELVIN SIGN is not ASCII `K` under the non-unicode `i` flag, so
    // neither `<tier>` nor the `\b` fallback may fold it (both regressed with
    // Rust's Unicode `(?i)`/`to_lowercase`).
    assert!(parse("<tier>\u{212A}</tier>").is_none());
    assert!(parse("choose \u{212A} now").is_none());
    // ASCII folding and the ASCII word-boundary fallback still work.
    assert!(parse("<tier>K</tier>").is_some_and(|tier| tier.equals_str("k")));
    assert!(parse("choose k now").is_some_and(|tier| tier.equals_str("k")));
    // U+017F LONG S must not match `[a-z]` through Unicode simple folding.
    assert!(parse("<tier>\u{017F}imple</tier>").is_none());
    assert!(parse("<tier>SIMPLE</tier>").is_some_and(|tier| tier.equals_str("simple")));
}

#[test]
fn subagent_detection_follows_js_non_unicode_case_folding() {
    let tools = |name: &str| JsValue::Array(vec![object(vec![("name", text(name))])]);
    let detect =
        |message: &str, tool: &str| detect_subagent(&[user_message(message)], &tools(tool), true);

    // U+017F LONG S is not ASCII `s`; Rust `(?i)` used to accept `taſk` as the
    // agent tool and clear missingAgentTool.
    let long_s = detect("hello", "ta\u{017F}k");
    assert!(long_s.missing_agent_tool);
    assert!(long_s.is_subagent);
    for tool in ["task", "TASK", "agent", "launch_agent", "spawn-agent"] {
        let found = detect("hello", tool);
        assert!(!found.missing_agent_tool, "{tool} is an agent tool");
    }
    assert!(detect("hello", "launch\u{017F}agent").missing_agent_tool);

    // The long-s tag is ordinary text: no hint, no taggedInUserMessage.
    let long_s_tag = detect(
        "<ccr-\u{017F}ubagent-model>p2/m2</ccr-\u{017F}ubagent-model>",
        "task",
    );
    assert!(!long_s_tag.tagged_in_user_message);
    assert_eq!(long_s_tag.model_hint, None);
    assert!(!long_s_tag.is_subagent);

    let ascii_tag = detect("<ccr-subagent-model>p2/m2</ccr-subagent-model>", "task");
    assert!(ascii_tag.tagged_in_user_message);
    assert_eq!(ascii_tag.model_hint, Some(JsString::from("p2/m2")));
    let upper_tag = detect("<CCR-SUBAGENT-MODEL>p2/m2</CCR-SUBAGENT-MODEL>", "task");
    assert_eq!(upper_tag.model_hint, Some(JsString::from("p2/m2")));

    // `.replace(...).trimEnd()` leaves an unmatched long-s tag in place (it
    // still trims the right edge).
    let stripped = scenario::strip_subagent_tag_from_messages(&[user_message(
        "  <pilotdeck-\u{017F}ubagent-model>p2/m2</pilotdeck-\u{017F}ubagent-model>  tail  ",
    )]);
    let text = stripped[0]
        .get("content")
        .and_then(|value| match value {
            JsValue::Array(items) => items.first(),
            _ => None,
        })
        .and_then(|block| block.get("text"))
        .and_then(JsValue::as_string)
        .and_then(|value| value.to_utf8().ok())
        .expect("text");
    assert_eq!(
        text,
        "  <pilotdeck-\u{017F}ubagent-model>p2/m2</pilotdeck-\u{017F}ubagent-model>  tail"
    );
}

#[test]
fn extract_last_user_message_skips_date_updates_and_joins_blocks() {
    let messages = vec![
        object(vec![
            ("role", text("user")),
            (
                "content",
                JsValue::Array(vec![
                    object(vec![("type", text("text")), ("text", text("  a  "))]),
                    object(vec![("type", text("text")), ("text", text("b"))]),
                ]),
            ),
        ]),
        object(vec![
            ("role", text("user")),
            (
                "content",
                JsValue::Array(vec![object(vec![
                    ("type", text("text")),
                    ("text", text("2026-01-01")),
                ])]),
            ),
            (
                "metadata",
                object(vec![
                    ("synthetic", JsValue::Bool(true)),
                    ("purpose", text("date_update")),
                ]),
            ),
        ]),
    ];
    assert_eq!(
        extract_last_user_message(&messages).map(|value| value.to_utf8().unwrap()),
        Some("a  \nb".into())
    );
    assert!(extract_last_user_message(&[]).is_none());
}

#[test]
fn short_continuation_uses_js_length_and_ascii_surface() {
    assert!(is_short_continuation(&"  go  ".into()));
    assert!(is_short_continuation(&"继续".into()));
    assert!(is_short_continuation(&"START".into()));
    // U+017F LONG S must not fold onto ASCII `s` (Rust `(?i)` used to match).
    assert!(!is_short_continuation(&"\u{017F}tart".into()));
    assert!(!is_short_continuation(&"go now please".into()));
    assert!(!is_short_continuation(&"a".repeat(31).into()));
}

#[test]
fn retry_policy_defaults_match_baseline() {
    let config = RouterConfig::default();
    let first = retry_decide(&config, "transient", 0.0, None, 0.0);
    assert!(first.retry);
    assert_eq!(first.max_attempts, 2.0);
    assert_eq!(first.delay_ms, 500.0);
    let jittered = retry_decide(&config, "transient", 0.0, None, 1.0);
    assert_eq!(jittered.delay_ms, 875.0);
    let capped = retry_decide(&config, "transient", 0.0, Some(99_999.0), 0.0);
    assert_eq!(capped.delay_ms, 8_000.0);
    let exhausted = retry_decide(&config, "transient", 2.0, None, 0.0);
    assert!(!exhausted.retry);
    let zero = retry_decide(&config, "zero_usage", 1.0, None, 0.0);
    assert!(zero.retry);
    assert_eq!(zero.delay_ms, 500.0);
}

#[tokio::test]
async fn cache_policy_switches_when_prefill_is_cheaper() {
    let mut pricing = crate::model_cost::PricingMap::new();
    pricing.insert(
        "old/m",
        crate::model_cost::ModelPricing {
            input: Some(100.0),
            output: None,
            cache_read: Some(100.0),
        },
    );
    pricing.insert(
        "new/m",
        crate::model_cost::ModelPricing {
            input: Some(0.1),
            output: None,
            cache_read: None,
        },
    );
    let config = RouterConfig {
        token_saver: Some(saver()),
        stats: StatsConfig {
            model_pricing: Some(pricing),
            ..StatsConfig::default()
        },
        ..RouterConfig::default()
    };
    let current = RouterModelRef::new("old".into(), "m".into());
    let next = RouterModelRef::new("new".into(), "m".into());
    let usage = object(vec![
        ("inputTokens", JsValue::Number(100.0)),
        ("cacheReadTokens", JsValue::Number(100.0)),
    ]);
    let result = policy::cache_policy_select(
        &config,
        Some(&current),
        &next,
        &[user_message("hello")],
        Some(&usage),
    )
    .await
    .expect("cache policy");
    assert_eq!(result.selection.provider.to_utf8().unwrap(), "new");
    let mutation = result.mutation.expect("switched mutation");
    assert_eq!(
        mutation
            .get("action")
            .and_then(JsValue::as_string)
            .map(|value| value.to_utf8().unwrap()),
        Some("switched".into())
    );
}

/// P1 regression: `cache_policy_select` used to run the o200k BPE inline on the
/// async reactor thread and bypass `cpu::run`'s 4-permit bound. Hold all four
/// permits, then prove the real decision path waits, while the raw estimator
/// (negative control) still runs — the estimate is gated by the call site, not
/// by the tokenizer itself. A regression to the inline call completes within the
/// timeout and fails the positive assertion.
#[tokio::test(flavor = "current_thread")]
async fn cache_policy_estimate_shares_the_cpu_permit_pool() {
    let mut pricing = crate::model_cost::PricingMap::new();
    pricing.insert(
        "old/m",
        crate::model_cost::ModelPricing {
            input: Some(100.0),
            output: None,
            cache_read: Some(100.0),
        },
    );
    pricing.insert(
        "new/m",
        crate::model_cost::ModelPricing {
            input: Some(0.1),
            output: None,
            cache_read: None,
        },
    );
    let config = RouterConfig {
        token_saver: Some(saver()),
        stats: StatsConfig {
            model_pricing: Some(pricing),
            ..StatsConfig::default()
        },
        ..RouterConfig::default()
    };
    let current = RouterModelRef::new("old".into(), "m".into());
    let next = RouterModelRef::new("new".into(), "m".into());
    let usage = object(vec![
        ("inputTokens", JsValue::Number(100.0)),
        ("cacheReadTokens", JsValue::Number(100.0)),
    ]);

    let started = Arc::new(AtomicUsize::new(0));
    let mut releases = Vec::new();
    let mut holders = Vec::new();
    for _ in 0..4 {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        releases.push(release);
        let started = started.clone();
        holders.push(tokio::spawn(async move {
            crate::runtime::model::cpu::run(move || {
                started.fetch_add(1, Ordering::SeqCst);
                let _ = wait.recv();
                Ok::<_, crate::host::HostError>(())
            })
            .await
        }));
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while started.load(Ordering::SeqCst) < 4 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "four cpu permits were not acquired"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    // Negative control: the raw estimator is not itself gated, so calling it
    // directly still succeeds while the whole pool is held. The positive
    // assertion below therefore proves `cache_policy_select` added the gate.
    let direct =
        tokio::task::spawn_blocking(|| policy::estimate_input_tokens(&[user_message("hello")]))
            .await
            .expect("direct estimate");
    assert!(direct > 0.0);

    let mut decision = tokio::spawn(async move {
        policy::cache_policy_select(
            &config,
            Some(&current),
            &next,
            &[user_message("hello")],
            Some(&usage),
        )
        .await
        .expect("cache policy")
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(500), &mut decision)
            .await
            .is_err(),
        "cache_policy_select must wait for a cpu permit while all four are held"
    );

    for release in releases {
        release.send(()).expect("release cpu holder");
    }
    for holder in holders {
        holder.await.expect("cpu holder task").expect("cpu holder");
    }
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), decision)
        .await
        .expect("cache policy resumes after a permit frees")
        .expect("cache policy task");
    assert_eq!(result.selection.provider.to_utf8().unwrap(), "new");
}

#[test]
fn session_store_expires_and_moves_lru_entries() {
    let clock = Arc::new(AtomicU64::new(0));
    let now = clock.clone();
    let store =
        session::SessionRouterStore::new(Arc::new(move || now.load(Ordering::SeqCst) as f64));
    store.set(SessionRoutingState {
        session_id: "s".into(),
        token_saver_tier: Some("simple".into()),
        ..Default::default()
    });
    assert!(store.get(&"s".into(), false).is_some());
    clock.store(3_600_001, Ordering::SeqCst);
    assert!(
        store.get(&"s".into(), false).is_none(),
        "TTL is 1h, strict less-than"
    );
}

#[tokio::test(start_paused = true)]
async fn classify_retries_parse_failure_then_succeeds() {
    let invoker = Invoker::new(vec![
        Step::Text("no tier".into()),
        Step::Text("<tier>complex</tier>".into()),
    ]);
    let decision = classify_and_route(
        &saver(),
        &[user_message("hi")],
        &invoker,
        None,
        None,
        &"s".into(),
    )
    .await
    .expect("no abort")
    .expect("decision");
    assert_eq!(decision.tier.to_utf8().unwrap(), "complex");
    assert_eq!(invoker.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn classify_empty_after_retries_falls_back() {
    let invoker = Invoker::new(vec![Step::Empty, Step::Empty, Step::Empty]);
    let decision = classify_and_route(
        &saver(),
        &[user_message("hi")],
        &invoker,
        None,
        None,
        &"s".into(),
    )
    .await
    .expect("no abort")
    .expect("decision");
    assert_eq!(decision.resolved_from, "fallback");
    assert_eq!(decision.failure_reason, Some("parse_error"));
    assert_eq!(invoker.calls(), 3);
    assert_eq!(decision.failure.expect("failure").attempts, 3.0);
}

#[tokio::test(start_paused = true)]
async fn classify_timeout_is_not_retried() {
    let mut config = saver();
    config.judge_timeout_ms = 500.0;
    let invoker = Invoker::new(vec![Step::Hang]);
    let decision = classify_and_route(
        &config,
        &[user_message("hi")],
        &invoker,
        None,
        None,
        &"s".into(),
    )
    .await
    .expect("no abort")
    .expect("decision");
    assert_eq!(decision.resolved_from, "fallback");
    assert_eq!(decision.failure_reason, Some("timeout"));
    assert_eq!(invoker.calls(), 1);
    let failure = decision.failure.expect("failure");
    assert_eq!(
        failure.code.as_ref().map(|code| code.to_utf8().unwrap()),
        Some("judge_timeout".into())
    );
}

#[test]
fn materialize_routes_cache_plan_and_clamps_output() {
    let invoker = Invoker::new(Vec::new());
    let decision = object(vec![
        ("provider", text("p")),
        ("model", text("s")),
        ("mutations", object(vec![])),
    ]);
    let request = object(vec![
        ("provider", text("p")),
        ("model", text("s")),
        ("messages", JsValue::Array(vec![user_message("hi")])),
        ("maxOutputTokens", JsValue::Number(1000.0)),
        (
            "cachePlan",
            object(vec![
                ("provider", text("p")),
                ("model", text("s")),
                ("messages", JsValue::Array(vec![JsValue::Number(0.0)])),
            ]),
        ),
        (
            "cacheBreakpoints",
            JsValue::Array(vec![JsValue::Number(0.0)]),
        ),
    ]);
    let result = policy::materialize_request(&invoker, &decision, &request);
    assert_eq!(
        result.get("maxOutputTokens").and_then(JsValue::as_number),
        Some(100.0)
    );
    assert_eq!(
        result.get("cacheBreakpoints").cloned(),
        Some(JsValue::Array(vec![JsValue::Number(0.0)]))
    );
    let foreign_plan = object(vec![
        ("provider", text("q")),
        ("model", text("s")),
        ("messages", JsValue::Array(vec![JsValue::Number(0.0)])),
    ]);
    let request = object(vec![
        ("provider", text("p")),
        ("model", text("s")),
        ("messages", JsValue::Array(vec![user_message("hi")])),
        ("cachePlan", foreign_plan),
    ]);
    let result = policy::materialize_request(&invoker, &decision, &request);
    assert!(result.get("cachePlan").is_none_or(JsValue::is_undefined));
    assert!(
        result
            .get("cacheBreakpoints")
            .is_none_or(JsValue::is_undefined)
    );
}

#[test]
fn config_reads_snapshot_router_only() {
    let router = object(vec![
        ("enabled", JsValue::Bool(true)),
        (
            "scenarios",
            object(vec![(
                "default",
                object(vec![("provider", text("p")), ("model", text("m"))]),
            )]),
        ),
        (
            "tokenSaver",
            object(vec![
                ("enabled", JsValue::Bool(true)),
                (
                    "judge",
                    object(vec![("provider", text("p")), ("model", text("j"))]),
                ),
                ("defaultTier", text("simple")),
                (
                    "tiers",
                    object(vec![(
                        "simple",
                        object(vec![(
                            "model",
                            object(vec![("provider", text("p")), ("model", text("s"))]),
                        )]),
                    )]),
                ),
            ]),
        ),
    ]);
    let snapshot = object(vec![("config", object(vec![("router", router)]))]);
    let config = RouterConfig::from_snapshot(&snapshot);
    assert_eq!(
        config
            .scenarios_default
            .as_ref()
            .map(|model| model.id.to_utf8().unwrap()),
        Some("p/m".into())
    );
    assert_eq!(
        config
            .token_saver
            .expect("token saver")
            .default_tier
            .to_utf8()
            .unwrap(),
        "simple"
    );
}

/// TS `optionalBaselineModel` (`parseRouterConfig.ts:791-811`) resolves the ref
/// against `modelConfig`; unknown refs must be dropped even by the convenience
/// projection that does not return diagnostics.
#[test]
fn unknown_baseline_model_is_dropped() {
    let snapshot = object(vec![(
        "config",
        object(vec![(
            "router",
            object(vec![(
                "stats",
                object(vec![(
                    "baselineModel",
                    object(vec![("provider", text("nope")), ("model", text("nope"))]),
                )]),
            )]),
        )]),
    )]);
    let config = RouterConfig::from_snapshot(&snapshot);
    assert_eq!(
        config
            .stats
            .baseline_model
            .as_ref()
            .map(|model| model.id.to_utf8().unwrap()),
        None
    );
}

#[tokio::test]
async fn invalidate_sticky_preserves_orchestration_tier() {
    let config = RouterConfig {
        scenarios_default: Some(RouterModelRef::new("p".into(), "s".into())),
        token_saver: Some(saver()),
        auto_orchestrate: Some(AutoOrchestrateConfig {
            enabled: true,
            trigger_tiers: vec!["simple".into()],
            subagent_max_tokens: None,
        }),
        ..RouterConfig::default()
    };
    let invoker = Arc::new(Invoker::new(vec![Step::Text("<tier>simple</tier>".into())]));
    let runtime = RouterRuntime::new(config, invoker).expect("runtime");
    let input = RouterDecisionInput {
        request: object(vec![
            ("provider", text("p")),
            ("model", text("s")),
            ("messages", JsValue::Array(vec![user_message("hi")])),
        ]),
        session_id: "s".into(),
        is_main_agent: true,
        abort: None,
        metadata: None,
    };
    let decision = runtime.decide(&input).await.expect("decision");
    assert_eq!(
        decision.get("orchestrating").and_then(JsValue::as_bool),
        Some(true)
    );
    let invalidated = runtime.invalidate_sticky(&"s".into());
    assert_eq!(
        invalidated
            .get("previousTier")
            .and_then(JsValue::as_string)
            .map(|value| value.to_utf8().unwrap()),
        Some("simple".into())
    );
    assert_eq!(
        invalidated.get("orchestrating").and_then(JsValue::as_bool),
        Some(true)
    );
}

#[test]
fn default_clock_is_real_wall_clock() {
    let deps = RouterDeps::new(Arc::new(Invoker::new(Vec::new())));
    let before = wall_clock();
    let value = (deps.clock)();
    let after = wall_clock();
    assert!(
        value >= before && value <= after,
        "default clock must be a live Date.now()-style millisecond clock: {before} <= {value} <= {after}"
    );
    assert!(
        value > 1_600_000_000_000.0,
        "default clock looks frozen at {value}"
    );
}

fn decision_input(messages: Vec<JsValue>, session_id: &str) -> RouterDecisionInput {
    RouterDecisionInput {
        request: object(vec![
            ("provider", text("p")),
            ("model", text("s")),
            ("messages", JsValue::Array(messages)),
        ]),
        session_id: session_id.into(),
        is_main_agent: true,
        abort: None,
        metadata: None,
    }
}

#[tokio::test]
async fn router_sticky_expires_after_injected_ttl() {
    let clock = Arc::new(AtomicU64::new(0));
    let now = clock.clone();
    let invoker = Arc::new(Invoker::new(vec![
        Step::Text("<tier>simple</tier>".into()),
        Step::Text("<tier>complex</tier>".into()),
    ]));
    let config = RouterConfig {
        scenarios_default: Some(RouterModelRef::new("p".into(), "s".into())),
        token_saver: Some(saver()),
        ..RouterConfig::default()
    };
    let deps = RouterDeps {
        clock: Arc::new(move || now.load(Ordering::SeqCst) as f64),
        ..RouterDeps::new(invoker.clone())
    };
    let runtime = RouterRuntime::with_deps(config, deps).expect("runtime");

    let first = runtime
        .decide(&decision_input(vec![user_message("hi")], "s"))
        .await
        .expect("first decision");
    assert_eq!(
        first
            .get("tokenSaverTier")
            .and_then(JsValue::as_string)
            .map(|value| value.to_utf8().unwrap()),
        Some("simple".into())
    );
    assert_eq!(invoker.calls(), 1);

    // Exactly 1h is still live (`expiresAt < now` is strict).
    clock.store(3_600_000, Ordering::SeqCst);
    let sticky = runtime
        .decide(&decision_input(
            vec![user_message("hi"), user_message("again")],
            "s",
        ))
        .await
        .expect("sticky decision");
    assert_eq!(invoker.calls(), 1, "1h-old sticky must still hit");
    assert_eq!(
        sticky
            .get("tokenSaverTier")
            .and_then(JsValue::as_string)
            .map(|value| value.to_utf8().unwrap()),
        Some("simple".into())
    );

    // One millisecond past the TTL re-judges. The 1h sticky hit above refreshed
    // the slot, so expire that refreshed window.
    clock.store(7_200_001, Ordering::SeqCst);
    let rejudged = runtime
        .decide(&decision_input(
            vec![user_message("hi"), user_message("again")],
            "s",
        ))
        .await
        .expect("rejudged decision");
    assert_eq!(invoker.calls(), 2, "expired sticky must be re-judged");
    assert_eq!(
        rejudged
            .get("tokenSaverTier")
            .and_then(JsValue::as_string)
            .map(|value| value.to_utf8().unwrap()),
        Some("complex".into())
    );
}

#[tokio::test(start_paused = true)]
async fn judge_timeout_does_not_abort_the_caller() {
    let mut config = saver();
    config.judge_timeout_ms = 500.0;
    let invoker = Invoker::new(vec![Step::Hang]);
    let caller = AbortFlag::new();
    let decision = classify_and_route(
        &config,
        &[user_message("hi")],
        &invoker,
        Some(caller.clone()),
        None,
        &"s".into(),
    )
    .await
    .expect("judge timeout is a fallback, not a caller abort")
    .expect("decision");
    assert_eq!(decision.failure_reason, Some("timeout"));
    assert!(
        !caller.is_aborted(),
        "P1-1 regression: judge timeout leaked into the caller's abort flag"
    );
    assert!(
        invoker
            .last_abort()
            .expect("judge received an abort flag")
            .is_aborted(),
        "the judge attempt's own controller must be aborted on timeout"
    );
}

#[tokio::test(start_paused = true)]
async fn caller_abort_is_forwarded_to_the_judge_attempt() {
    let invoker = Arc::new(Invoker::new(vec![Step::Hang]));
    let caller = AbortFlag::new();
    let caller_task = caller.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        caller_task.abort();
    });
    let error = classify_and_route(
        &saver(),
        &[user_message("hi")],
        invoker.as_ref(),
        Some(caller.clone()),
        None,
        &"s".into(),
    )
    .await
    .expect_err("caller abort must surface as an error");
    assert_eq!(
        error.message.to_utf8().unwrap_or_default(),
        "This operation was aborted"
    );
    assert!(caller.is_aborted());
    assert!(
        invoker
            .last_abort()
            .expect("judge received an abort flag")
            .is_aborted(),
        "the caller's abort must be forwarded to the judge's child flag"
    );
}

#[test]
fn sanitize_failure_message_matches_js_regex_semantics() {
    let sanitize = |value: &str| {
        classify::sanitize_failure_message(&JsString::from(value))
            .to_utf8()
            .expect("sanitized output is valid UTF-8")
    };
    // JS `\b` is ASCII-word based even before CJK text.
    assert_eq!(sanitize("无效token=sk-abc"), "无效token=<redacted>");
    assert_eq!(sanitize("invalid token=sk-abc"), "invalid token=<redacted>");
    assert_eq!(sanitize("_token=sk-abc"), "_token=sk-abc");
    // JS `\s` includes U+FEFF and excludes U+0085.
    assert_eq!(sanitize("x\u{FEFF}token=sk-abc"), "x token=<redacted>");
    assert_eq!(sanitize("token\u{0085}=sk-abc"), "token\u{0085}=sk-abc");
    assert_eq!(sanitize("\u{0085} x \u{0085}"), "\u{0085} x \u{0085}");
    assert_eq!(
        sanitize("\u{180E} token=sk \u{180E}"),
        "\u{180E} token=<redacted> \u{180E}"
    );
    assert_eq!(sanitize("Bearer \u{FEFF} sk-secret"), "Bearer <redacted>");
    // Non-ASCII characters never case-fold onto ASCII (ES non-unicode rule).
    assert_eq!(
        sanitize("pa\u{017F}\u{017F}word=sk-abc"),
        "pa\u{017F}\u{017F}word=sk-abc"
    );
    assert_eq!(sanitize("to\u{212A}en=sk-abc"), "to\u{212A}en=sk-abc");
    // `\s+` collapse happens before `trim`, and JS trim includes U+FEFF.
    assert_eq!(
        sanitize("\u{FEFF} token=sk \u{0085}"),
        "token=<redacted> \u{0085}"
    );
    assert_eq!(
        sanitize("API_KEY = s3cr3t; next"),
        "API_KEY=<redacted>; next"
    );
    assert_eq!(
        sanitize("authorization: bearer abc.def"),
        "authorization: bearer <redacted>"
    );
    // Greedy/backtracking edges and keyword-spelling variants from Node.
    assert_eq!(sanitize("Bearer    "), "Bearer");
    assert_eq!(sanitize("bearer  ,x"), "bearer ,x");
    assert_eq!(sanitize("token==x"), "token=<redacted>");
    assert_eq!(sanitize("token: :x"), "token=<redacted>");
    assert_eq!(sanitize("api-key: v"), "api-key=<redacted>");
    assert_eq!(sanitize("api_key=v"), "api_key=<redacted>");
    assert_eq!(sanitize("apikey = v"), "apikey=<redacted>");
    assert_eq!(sanitize("API KEY=v"), "API KEY=v");
    assert_eq!(sanitize("tokens=1"), "tokens=1");
    assert_eq!(sanitize("secretary=1"), "secretary=1");
    assert_eq!(sanitize("TOKEN=x"), "TOKEN=<redacted>");
    // Greek Kappa is not ASCII `K`, so the non-unicode `i` flag cannot fold it.
    assert_eq!(sanitize("TO\u{039A}EN=x"), "TO\u{039A}EN=x");
    assert_eq!(sanitize("bearer\u{FEFF}x"), "bearer <redacted>");
    assert_eq!(
        sanitize("authorization : bearer x ;"),
        "authorization : bearer <redacted> ;"
    );
    assert_eq!(
        sanitize("xauthorization: bearer tok"),
        "xauthorization: bearer <redacted>"
    );

    // Lone surrogates and the 300 UTF-16-code-unit slice are preserved.
    let mut units = "bad".encode_utf16().collect::<Vec<_>>();
    units.push(0xd800);
    units.extend("token=sk-abc".encode_utf16());
    let output = classify::sanitize_failure_message(&JsString::from_utf16(units));
    let mut expected = "bad".encode_utf16().collect::<Vec<_>>();
    expected.push(0xd800);
    expected.extend("token=<redacted>".encode_utf16());
    assert_eq!(output.units(), expected.as_slice());

    let emoji = format!("{}😀", "x".repeat(299));
    let output = classify::sanitize_failure_message(&JsString::from(emoji.as_str()));
    assert_eq!(output.units().len(), 300);
    assert_eq!(output.units()[299], 0xd83d);
}

/// A stream that reports no usage and ends in a terminal, non-retryable error
/// after one text delta. With the V8 string limit shrunk, `final_usage` throws
/// while appending the buffered delta.
struct FailingStreamInvoker;
impl RouterModelInvocation for FailingStreamInvoker {
    fn get_capabilities(
        &self,
        _: &JsString,
        _: &JsString,
    ) -> Result<JsValue, crate::host::ModuleError> {
        Ok(object(vec![("maxOutputTokens", JsValue::Number(64.0))]))
    }
    fn get_multimodal(
        &self,
        _: &JsString,
        _: &JsString,
    ) -> Result<JsValue, crate::host::ModuleError> {
        Ok(object(vec![("input", JsValue::Array(vec![text("text")]))]))
    }
    fn get_provider_protocol(&self, _: &JsString) -> JsString {
        "anthropic".into()
    }
    fn get_provider_generation(&self, _: &JsString) -> JsValue {
        JsValue::Undefined
    }
    fn stream<'a>(
        &'a self,
        _: &'a JsValue,
        _: &'a StreamOptions,
        sink: &'a mut (dyn FnMut(StreamOutput) + Send),
    ) -> RouterStreamFuture<'a> {
        Box::pin(async move {
            sink(StreamOutput::Event(object(vec![
                ("type", text("text_delta")),
                ("text", text("abcdef")),
            ])));
            sink(StreamOutput::Event(object(vec![
                ("type", text("error")),
                (
                    "error",
                    object(vec![
                        ("code", text("server_error")),
                        ("message", text("boom")),
                        ("retryable", JsValue::Bool(false)),
                    ]),
                ),
            ])));
            Ok(())
        })
    }
}

/// Gate P2 (concurrency #1): the terminal-failure branch used `?` on
/// `final_usage`, so a Rust-side estimation failure skipped the buffered-event
/// flush and the terminal `error` event. Negative control: restore `?` and the
/// sink stays empty while `execute` returns Err.
#[tokio::test(flavor = "current_thread")]
async fn terminal_failure_still_flushes_events_when_usage_estimation_throws() {
    let _limit = crate::runtime::model::http::LimitGuard::set(4);
    let config = RouterConfig {
        enabled: true,
        ..RouterConfig::default()
    };
    let mut deps = RouterDeps::new(Arc::new(FailingStreamInvoker));
    deps.clock = Arc::new(|| 1_700_000_000_000.0);
    let router = RouterRuntime::with_deps(config, deps).expect("runtime");
    let decision = object(vec![
        ("provider", text("p")),
        ("model", text("s")),
        ("scenarioType", text("default")),
        ("isSubagent", JsValue::Bool(false)),
        ("orchestrating", JsValue::Bool(false)),
        ("resolvedFrom", text("scenario")),
        ("mutations", object(vec![])),
    ]);
    let request = object(vec![
        ("provider", text("p")),
        ("model", text("s")),
        ("messages", JsValue::Array(vec![user_message("hi")])),
        ("maxOutputTokens", JsValue::Number(100.0)),
    ]);
    let ctx = RouterExecuteContext {
        session_id: "s".into(),
        turn_id: "t".into(),
        ..Default::default()
    };
    let mut events = Vec::new();
    router
        .execute(&decision, &request, &ctx, &mut |output| events.push(output))
        .await
        .expect("execute must not abort the stream on a usage-estimation error");
    let kinds = events
        .iter()
        .map(|event| {
            field(event, "type")
                .as_string()
                .and_then(|value| value.to_utf8().ok())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    assert!(kinds.iter().any(|kind| kind == "text_delta"), "{kinds:?}");
    assert_eq!(kinds.last().map(String::as_str), Some("error"), "{kinds:?}");
}
