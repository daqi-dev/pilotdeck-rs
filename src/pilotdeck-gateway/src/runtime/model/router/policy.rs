//! `router/policy/*`: cache-aware switching, orchestration admission, request
//! materialization and retry admission.
use super::{
    AutoOrchestrateConfig, RouterConfig, RouterModelInvocation, RouterModelRef,
    config::LITELLM_RETRY_JITTER, field, js_text, number, object_field,
};
use crate::host::HostError;
use crate::model_cost::{calculate_cache_read_cost, calculate_input_cost};
use pilotdeck_protocol::{
    JsObject, JsString, JsValue,
    js_math::{max as js_math_max, min as js_math_min},
    stringify_json,
};

pub struct CachePolicyResult {
    pub selection: RouterModelRef,
    pub mutation: Option<JsValue>,
}

pub async fn cache_policy_select(
    config: &RouterConfig,
    current: Option<&RouterModelRef>,
    next: &RouterModelRef,
    messages: &[JsValue],
    last_usage: Option<&JsValue>,
) -> Result<CachePolicyResult, HostError> {
    let cache_aware = config
        .token_saver
        .as_ref()
        .map(|saver| saver.cache_aware.clone());
    let Some(cache_aware) = cache_aware.filter(|cache| cache.enabled) else {
        return Ok(CachePolicyResult {
            selection: next.clone(),
            mutation: None,
        });
    };
    let Some(current) = current else {
        return Ok(CachePolicyResult {
            selection: next.clone(),
            mutation: None,
        });
    };
    if current.provider == next.provider && current.model == next.model {
        return Ok(CachePolicyResult {
            selection: next.clone(),
            mutation: None,
        });
    }

    // The o200k BPE runs on the shared CPU pool, never on the async reactor
    // thread. `messages` is cloned for the blocking closure; the copy is cheap
    // next to tokenization and keeps the estimate inside the 4-permit bound.
    let messages = messages.to_vec();
    let estimated_input_tokens =
        super::super::cpu::run(move || Ok::<_, HostError>(estimate_input_tokens(&messages)))
            .await?;
    let observed_input_tokens = last_usage
        .and_then(|usage| usage.get("inputTokens"))
        .and_then(JsValue::as_number)
        .unwrap_or(0.0);
    let observed_cache_read_tokens = last_usage
        .and_then(|usage| usage.get("cacheReadTokens"))
        .and_then(JsValue::as_number)
        .unwrap_or(0.0);
    let observed_cache_hit_ratio = if observed_input_tokens > 0.0 {
        (observed_cache_read_tokens / observed_input_tokens).clamp(0.0, 1.0)
    } else {
        0.0
    };
    if observed_cache_hit_ratio <= 0.0 {
        return Ok(CachePolicyResult {
            selection: next.clone(),
            mutation: None,
        });
    }

    let estimated_cache_read_tokens = (estimated_input_tokens * observed_cache_hit_ratio).floor();
    let estimated_uncached_tokens = (estimated_input_tokens - estimated_cache_read_tokens).max(0.0);
    let pricing = config.stats.model_pricing.as_ref();
    let current_provider = current.provider.to_utf8().unwrap_or_default();
    let current_model = current.model.to_utf8().unwrap_or_default();
    let next_provider = next.provider.to_utf8().unwrap_or_default();
    let next_model = next.model.to_utf8().unwrap_or_default();
    let cached_cost = calculate_cache_read_cost(
        estimated_cache_read_tokens,
        &current_provider,
        &current_model,
        pricing,
    ) + calculate_input_cost(
        estimated_uncached_tokens,
        &current_provider,
        &current_model,
        pricing,
    );
    let prefill_cost =
        calculate_input_cost(estimated_input_tokens, &next_provider, &next_model, pricing);

    let required_savings = cached_cost * cache_aware.min_savings_ratio;
    let should_switch = prefill_cost + f64::EPSILON < cached_cost - required_savings;
    let from = format!("{current_provider}/{current_model}");
    let to = format!("{next_provider}/{next_model}");
    let mut mutation = JsObject::new();
    mutation.insert(
        "action".into(),
        if should_switch {
            "switched".into()
        } else {
            "kept_sticky".into()
        },
    );
    mutation.insert("from".into(), JsValue::String(JsString::from(from)));
    mutation.insert("to".into(), JsValue::String(JsString::from(to)));
    mutation.insert("cachedCost".into(), JsValue::Number(cached_cost));
    mutation.insert("prefillCost".into(), JsValue::Number(prefill_cost));
    mutation.insert(
        "estimatedInputTokens".into(),
        JsValue::Number(estimated_input_tokens),
    );
    Ok(CachePolicyResult {
        selection: if should_switch {
            next.clone()
        } else {
            current.clone()
        },
        mutation: Some(JsValue::Object(mutation)),
    })
}

/// `countMessagesTokens`: concatenate canonical text/tool blocks with `\n` and
/// run the shared o200k tokenizer once.
pub fn estimate_input_tokens(messages: &[JsValue]) -> f64 {
    let mut chunks: Vec<JsString> = Vec::new();
    for message in messages {
        let JsValue::Array(blocks) = field(message, "content") else {
            continue;
        };
        for block in blocks {
            let kind = field(block, "type")
                .as_string()
                .map(|value| value.to_utf8().unwrap_or_default());
            match kind.as_deref() {
                Some("text" | "thinking") => {
                    if let Some(text) = js_text(field(block, "text")) {
                        chunks.push(text.clone());
                    }
                }
                Some("tool_call") => {
                    let input = field(block, "input");
                    if !input.is_undefined() {
                        if let Some(text) = input.as_string() {
                            chunks.push(text.clone());
                        } else if let Ok(Some(json)) = stringify_json(input) {
                            chunks.push(JsString::from(json));
                        }
                    }
                }
                Some("tool_result") => {
                    chunks.push(flatten_tool_result(field(block, "content")));
                }
                _ => {}
            }
        }
    }
    let mut units = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        if index > 0 {
            units.push(10);
        }
        units.extend_from_slice(chunk.units());
    }
    let text = JsString::from_utf16(units);
    pilotdeck_tokenizer::count_tokens(&text).unwrap_or(0) as f64
}

fn push_str(units: &mut Vec<u16>, text: &str) {
    units.extend_from_slice(&text.encode_utf16().collect::<Vec<_>>());
}

fn flatten_tool_result(content: &JsValue) -> JsString {
    let JsValue::Array(items) = content else {
        return JsString::default();
    };
    let mut units = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            units.push(10);
        }
        let kind = field(item, "type")
            .as_string()
            .map(|value| value.to_utf8().unwrap_or_default());
        match kind.as_deref() {
            Some("text") => {
                if let Some(text) = js_text(field(item, "text")) {
                    units.extend_from_slice(text.units());
                }
            }
            Some("image") => {
                let source = field(item, "source")
                    .as_string()
                    .map(|value| value.to_utf8().unwrap_or_default());
                let mime = field(item, "mimeType")
                    .as_string()
                    .map(|value| value.to_utf8().unwrap_or_default())
                    .unwrap_or_default();
                let data = field(item, "data").as_string();
                if source.as_deref() == Some("url") {
                    push_str(&mut units, &format!("[Image: {mime}, "));
                    units.extend_from_slice(data.map(JsString::units).unwrap_or_default());
                    push_str(&mut units, "]");
                } else {
                    let length = data.map(|value| value.units().len()).unwrap_or(0);
                    push_str(
                        &mut units,
                        &format!("[Image: {mime}, {length} base64 characters]"),
                    );
                }
            }
            Some("pdf") => {
                let mime = field(item, "mimeType")
                    .as_string()
                    .map(|value| value.to_utf8().unwrap_or_default())
                    .unwrap_or_default();
                let length = field(item, "data")
                    .as_string()
                    .map(|value| value.units().len())
                    .unwrap_or(0);
                let pages = number(field(item, "pages")).filter(|pages| *pages != 0.0);
                let suffix = pages
                    .map(|pages| format!(", {pages} pages"))
                    .unwrap_or_default();
                push_str(
                    &mut units,
                    &format!("[PDF: {mime}, {length} base64 characters{suffix}]"),
                );
            }
            _ => {}
        }
    }
    JsString::from_utf16(units)
}

pub struct OrchestrationResult {
    pub mutations: JsObject,
    pub applied: bool,
}
pub fn orchestration_decide(
    config: Option<&AutoOrchestrateConfig>,
    is_main_agent: bool,
    tier: Option<&JsString>,
    already_orchestrating: bool,
) -> OrchestrationResult {
    let none = || OrchestrationResult {
        mutations: JsObject::new(),
        applied: false,
    };
    let Some(config) = config else {
        return none();
    };
    if !config.enabled || !is_main_agent {
        return none();
    }
    if !already_orchestrating
        && !config.trigger_tiers.is_empty()
        && tier.is_none_or(|tier| !config.trigger_tiers.iter().any(|value| value == tier))
    {
        return none();
    }
    let mut activated = JsObject::new();
    activated.insert(
        "tier".into(),
        JsValue::String(tier.cloned().unwrap_or_else(|| JsString::from("main"))),
    );
    activated.insert("continued".into(), JsValue::Bool(already_orchestrating));
    let mut mutations = JsObject::new();
    mutations.insert("orchestrationActivated".into(), JsValue::Object(activated));
    OrchestrationResult {
        mutations,
        applied: true,
    }
}

/// `createNativeRouterRequestMaterializer.materialize`.
pub fn materialize_request(
    invoker: &dyn RouterModelInvocation,
    decision: &JsValue,
    request: &JsValue,
) -> JsValue {
    let Some(request) = request.as_object() else {
        return request.clone();
    };
    let patch = object_field(decision, "requestPatch")
        .cloned()
        .unwrap_or_default();
    let mut messages = patch
        .get("messages")
        .cloned()
        .or_else(|| request.get("messages").cloned())
        .unwrap_or(JsValue::Undefined);
    let stripped = object_field(decision, "mutations")
        .and_then(|mutations| mutations.get("subagentTagStripped"))
        .and_then(JsValue::as_bool)
        .unwrap_or(false);
    if stripped && let JsValue::Array(items) = &messages {
        messages = JsValue::Array(super::scenario::strip_subagent_tag_from_messages(items));
    }

    // `RouterRequestMaterializer.ts:30-31`: `provider`/`model` are the
    // decision's raw route values — an explicit override can leave either
    // `undefined` or pass a non-string through, and the materialized request
    // carries it as-is (serialization drops `undefined`).
    let provider = field(decision, "provider").clone();
    let model = field(decision, "model").clone();
    let cache_plan = request.get("cachePlan").cloned();
    let routed_cache_plan = cache_plan.as_ref().and_then(|plan| {
        let plan = plan.as_object()?;
        // `RouterRequestMaterializer.ts:22-25`: strict equality against the
        // decision's route values, raw types included.
        let provider_matches = plan
            .get("provider")
            .is_none_or(|value| value.is_undefined() || route_strictly_equals(value, &provider));
        let model_matches = plan
            .get("model")
            .is_none_or(|value| value.is_undefined() || route_strictly_equals(value, &model));
        (provider_matches && model_matches).then(|| plan.clone())
    });
    let has_cache_plan = cache_plan.as_ref().is_some_and(|plan| !plan.is_undefined());
    let cache_breakpoints = if has_cache_plan {
        routed_cache_plan
            .as_ref()
            .and_then(|plan| plan.get("messages"))
            .cloned()
            .unwrap_or(JsValue::Undefined)
    } else {
        request
            .get("cacheBreakpoints")
            .cloned()
            .unwrap_or(JsValue::Undefined)
    };

    let mut result = request.clone();
    for (key, value) in patch.entries() {
        result.insert(key.clone(), value.clone());
    }
    result.insert("provider".into(), provider.clone());
    result.insert("model".into(), model.clone());
    result.insert("messages".into(), messages);
    result.insert("cacheBreakpoints".into(), cache_breakpoints);
    result.insert(
        "cachePlan".into(),
        routed_cache_plan
            .map(JsValue::Object)
            .unwrap_or(JsValue::Undefined),
    );

    let requested = result.get("maxOutputTokens").and_then(JsValue::as_number);
    let Some(requested) = requested else {
        return JsValue::Object(result);
    };
    // `clampMaxOutputTokensToModelCap` (`RouterRequestMaterializer.ts:51-59`):
    // the capability read indexes the config by the raw route's property-key
    // form and an unknown route — or a route whose key conversion throws — is
    // caught here, leaving the request alone.
    if let (Ok(provider_key), Ok(model_key)) = (
        crate::js_value::property_key(&provider),
        crate::js_value::property_key(&model),
    ) && let Ok(capabilities) = invoker.get_capabilities(&provider_key, &model_key)
    {
        let cap = capabilities
            .get("maxOutputTokens")
            .and_then(JsValue::as_number);
        if let Some(cap) = cap.filter(|cap| cap.is_finite() && *cap > 0.0 && requested > *cap) {
            result.insert("maxOutputTokens".into(), JsValue::Number(cap));
        }
    }
    JsValue::Object(result)
}

/// JS `===` between a cache-plan route and the decision's raw route
/// (`RouterRequestMaterializer.ts:23-24`). Primitives compare by value —
/// except `NaN`, which never equals itself — and objects/arrays by reference
/// identity. [`JsValue`] carries no identity, and both operands cross a
/// serialization boundary before they reach this port (the module wire or the
/// replay oracle), where references are severed: two containers are never the
/// same object on inputs this port can receive, so they never strictly equal.
/// TS keeps the cache plan only when one object aliases the other
/// (`context.modelOverride.provider === request.cachePlan.provider`); the
/// fixed baseline never produces that — `AgentModelOverride` is
/// `{provider: string, model: string}` (`agent/protocol/input.ts:16-18`) and
/// the request's cache plan is assembled from config strings
/// (`modelRequestAssembly.ts:59-78`) — so distinct same-shaped containers
/// clearing the plan is the only reachable shape, matching TS.
fn route_strictly_equals(value: &JsValue, route: &JsValue) -> bool {
    match (value, route) {
        (JsValue::Object(_), _)
        | (_, JsValue::Object(_))
        | (JsValue::Array(_), _)
        | (_, JsValue::Array(_)) => false,
        (JsValue::Number(a), JsValue::Number(b)) => a == b && !a.is_nan(),
        _ => value == route,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RetryDecision {
    pub retry: bool,
    pub max_attempts: f64,
    pub delay_ms: f64,
}
pub fn retry_decide(
    config: &RouterConfig,
    kind: &str,
    attempt: f64,
    retry_after_ms: Option<f64>,
    random: f64,
) -> RetryDecision {
    if kind == "zero_usage" {
        let max_attempts = js_math_max(config.zero_usage_retry.max_attempts, 1.0);
        return RetryDecision {
            retry: config.zero_usage_retry.enabled && attempt < max_attempts,
            max_attempts,
            delay_ms: 500.0 * js_math_max(attempt, 0.0),
        };
    }
    let transient = &config.transient_retry;
    let transient_max_attempts = js_math_max(transient.max_attempts, 1.0);
    if !transient.enabled || attempt >= transient_max_attempts {
        return RetryDecision {
            retry: false,
            max_attempts: transient_max_attempts,
            delay_ms: 0.0,
        };
    }
    let delay_ms = match retry_after_ms {
        Some(value) => js_math_min(value, transient.max_delay_ms),
        None => {
            let deterministic = transient.base_delay_ms * (attempt + 1.0);
            let jitter =
                deterministic * LITELLM_RETRY_JITTER * js_math_max(0.0, js_math_min(1.0, random));
            js_math_min(deterministic + jitter, transient.max_delay_ms)
        }
    };
    RetryDecision {
        retry: true,
        max_attempts: transient_max_attempts,
        delay_ms,
    }
}
