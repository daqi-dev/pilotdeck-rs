use super::{Diagnostics, model::Provider, value::*};
use pilotdeck_protocol::{JsString, JsValue};
fn shape(v: &JsValue, d: &mut Diagnostics, code: &str, msg: &str, path: &str) -> bool {
    if v.is_undefined() {
        false
    } else if !record(v) {
        d.fatal(code, msg, path);
        false
    } else {
        true
    }
}
fn reference(
    v: &JsValue,
    path: &str,
    p: &[Provider],
    d: &mut Diagnostics,
) -> Option<(JsString, JsString)> {
    let Some(s) = v.as_string().filter(|s| !s.is_empty()) else {
        d.fatal(
            "ROUTER_REF_INVALID",
            format!("{path} must be a non-empty provider/model string."),
            path,
        );
        return None;
    };
    let Some(i) = s
        .units()
        .iter()
        .position(|u| *u == 47)
        .filter(|i| *i > 0 && *i + 1 < s.units().len())
    else {
        d.fatal(
            "ROUTER_REF_FORMAT",
            format!(
                "{path} must use provider/model format; got {}.",
                js_string(v)
            ),
            path,
        );
        return None;
    };
    let a = JsString::from_utf16(s.units()[..i].to_vec());
    let b = JsString::from_utf16(s.units()[i + 1..].to_vec());
    let at = js_string(&JsValue::String(a.clone()));
    let Some(p) = p.iter().find(|p| p.id == a) else {
        d.fatal(
            "ROUTER_REF_PROVIDER_NOT_FOUND",
            format!("{path} references unknown provider {at}."),
            path,
        );
        return None;
    };
    if !p.models.iter().any(|m| m.id == b) {
        d.fatal(
            "ROUTER_REF_MODEL_NOT_FOUND",
            format!(
                "{path} references unknown model {} for provider {at}.",
                js_string(&JsValue::String(b))
            ),
            path,
        );
        return None;
    }
    Some((a, b))
}
fn bool_enabled(v: &JsValue, d: &mut Diagnostics, code: &str, msg: &str, path: &str) -> bool {
    if !v.is_undefined() && v.as_bool().is_none() {
        d.fatal(code, msg, path);
    }
    v.as_bool() != Some(false)
}
pub(super) fn parse(
    raw: &JsValue,
    p: &[Provider],
    agent: &(JsString, JsString),
    d: &mut Diagnostics,
) -> bool {
    if !shape(
        raw,
        d,
        "ROUTER_CONFIG_INVALID",
        "router config must be an object.",
        "router",
    ) {
        return false;
    }
    if !bool_enabled(
        field(raw, "enabled"),
        d,
        "ROUTER_ENABLED_INVALID",
        "router.enabled must be a boolean.",
        "router.enabled",
    ) {
        return false;
    }
    let scenarios = field(raw, "scenarios");
    let mut default = None;
    if shape(
        scenarios,
        d,
        "ROUTER_SCENARIOS_INVALID",
        "router.scenarios must be an object.",
        "router.scenarios",
    ) {
        default = reference(
            field(scenarios, "default"),
            "router.scenarios.default",
            p,
            d,
        );
    }
    let fallback = field(raw, "fallback");
    if shape(
        fallback,
        d,
        "ROUTER_FALLBACK_INVALID",
        "router.fallback must be an object keyed by scenario type.",
        "router.fallback",
    ) {
        for (k, v) in entries(fallback) {
            let k = js_string(&JsValue::String(k.clone()));
            let path = format!("router.fallback.{k}");
            if k == "maxFallbacks" {
                if !number(v, false, true) {
                    d.fatal(
                        "ROUTER_FALLBACK_MAX_FALLBACKS_INVALID",
                        "router.fallback.maxFallbacks must be a non-negative integer.",
                        &path,
                    );
                }
                continue;
            }
            if !["default", "subagent", "explicit"].contains(&k.as_str()) {
                d.warn(
                    "ROUTER_FALLBACK_UNKNOWN_SCENARIO",
                    format!("Unknown scenario type {k} in router.fallback."),
                    &path,
                );
                continue;
            }
            if let JsValue::Array(a) = v {
                for (i, v) in a.iter().enumerate() {
                    reference(v, &format!("{path}[{i}]"), p, d);
                }
            } else {
                d.fatal(
                    "ROUTER_FALLBACK_NOT_ARRAY",
                    format!("{path} must be an array of provider/model strings."),
                    &path,
                );
            }
        }
    }
    let zero = field(raw, "zeroUsageRetry");
    if shape(
        zero,
        d,
        "ROUTER_ZERO_USAGE_RETRY_INVALID",
        "router.zeroUsageRetry must be an object.",
        "router.zeroUsageRetry",
    ) {
        let n = field(zero, "maxAttempts");
        if !n.is_undefined() && !number(n, false, true) {
            d.fatal(
                "ROUTER_ZERO_USAGE_RETRY_MAX_ATTEMPTS_INVALID",
                "maxAttempts must be a non-negative integer.",
                "router.zeroUsageRetry.maxAttempts",
            );
        }
    }
    let transient = field(raw, "transientRetry");
    if shape(
        transient,
        d,
        "ROUTER_TRANSIENT_RETRY_INVALID",
        "router.transientRetry must be an object.",
        "router.transientRetry",
    ) {
        bool_enabled(
            field(transient, "enabled"),
            d,
            "ROUTER_TRANSIENT_RETRY_ENABLED_INVALID",
            "enabled must be a boolean.",
            "router.transientRetry.enabled",
        );
        for (key, int) in [
            ("maxAttempts", true),
            ("baseDelayMs", false),
            ("maxDelayMs", false),
        ] {
            let v = field(transient, key);
            if !v.is_undefined() && !number(v, false, int) {
                d.fatal(
                    "ROUTER_TRANSIENT_RETRY_VALUE_INVALID",
                    format!(
                        "{key} must be a non-negative {}.",
                        if int { "integer" } else { "number" }
                    ),
                    &format!("router.transientRetry.{key}"),
                );
            }
        }
    }
    let tiers = token_saver(field(raw, "tokenSaver"), p, d);
    auto_orchestrate(field(raw, "autoOrchestrate"), &tiers, d);
    stats(field(raw, "stats"), p, d);
    let custom = field(raw, "customRouter");
    if !custom.is_undefined()
        && (!record(custom)
            || field(custom, "extensionId")
                .as_string()
                .is_none_or(|s| s.is_empty()))
    {
        d.fatal(
            "ROUTER_CUSTOM_ROUTER_INVALID",
            "router.customRouter must declare an extensionId string.",
            "router.customRouter",
        );
    }
    if let Some((a, b)) = default.filter(|s| s != agent) {
        let previous = format!(
            "{}/{}",
            js_string(&JsValue::String(a)),
            js_string(&JsValue::String(b))
        );
        let current = format!(
            "{}/{}",
            js_string(&JsValue::String(agent.0.clone())),
            js_string(&JsValue::String(agent.1.clone()))
        );
        d.warn("CONFIG_MODEL_CONFLICT",format!("agent.model ({current}) and router.scenarios.default ({previous}) disagree. Using agent.model and overriding router.scenarios.default at runtime. Update the yaml to silence this warning."),"agent.model");
    }
    true
}
fn token_saver(raw: &JsValue, p: &[Provider], d: &mut Diagnostics) -> Vec<JsString> {
    if !shape(
        raw,
        d,
        "ROUTER_TOKEN_SAVER_INVALID",
        "router.tokenSaver must be an object.",
        "router.tokenSaver",
    ) {
        return vec![];
    }
    if !bool_enabled(
        field(raw, "enabled"),
        d,
        "ROUTER_TOKEN_SAVER_ENABLED_INVALID",
        "tokenSaver.enabled must be a boolean.",
        "router.tokenSaver.enabled",
    ) {
        return vec![];
    }
    if reference(field(raw, "judge"), "router.tokenSaver.judge", p, d).is_none() {
        return vec![];
    }
    let tiers = field(raw, "tiers");
    if !record(tiers) || entries(tiers).is_empty() {
        d.fatal(
            "ROUTER_TOKEN_SAVER_TIERS_EMPTY",
            "router.tokenSaver.tiers must be a non-empty object.",
            "router.tokenSaver.tiers",
        );
        return vec![];
    }
    let mut names = vec![];
    for (name, body) in entries(tiers) {
        let path = format!(
            "router.tokenSaver.tiers.{}",
            js_string(&JsValue::String(name.clone()))
        );
        if !record(body) {
            d.fatal(
                "ROUTER_TOKEN_SAVER_TIER_INVALID",
                format!(
                    "tier {} must be an object with model.",
                    js_string(&JsValue::String(name.clone()))
                ),
                &path,
            );
            continue;
        }
        if reference(field(body, "model"), &format!("{path}.model"), p, d).is_some() {
            names.push(name.clone());
        }
    }
    if names.is_empty() {
        return names;
    }
    if let Some(default) = field(raw, "defaultTier").as_string()
        && !names.contains(default)
    {
        d.fatal(
            "ROUTER_TOKEN_SAVER_DEFAULT_TIER_UNKNOWN",
            format!(
                "defaultTier {} is not present in tiers.",
                js_string(&JsValue::String(default.clone()))
            ),
            "router.tokenSaver.defaultTier",
        );
    }
    let rules = field(raw, "rules");
    if !rules.is_undefined() && !strings(rules) {
        d.fatal(
            "ROUTER_TOKEN_SAVER_RULES_INVALID",
            "router.tokenSaver.rules must be an array of strings.",
            "router.tokenSaver.rules",
        );
    }
    let sub = field(raw, "subagent");
    if shape(
        sub,
        d,
        "ROUTER_TOKEN_SAVER_SUBAGENT_INVALID",
        "router.tokenSaver.subagent must be an object.",
        "router.tokenSaver.subagent",
    ) {
        let policy = field(sub, "policy");
        if !eq(policy, "skip") && !eq(policy, "judge") {
            d.fatal(
                "ROUTER_TOKEN_SAVER_SUBAGENT_POLICY_INVALID",
                "subagent.policy must be one of skip / judge.",
                "router.tokenSaver.subagent.policy",
            );
        }
    }
    let timeout = field(raw, "judgeTimeoutMs");
    if !timeout.is_undefined() && !number(timeout, true, true) {
        d.fatal(
            "ROUTER_TOKEN_SAVER_JUDGE_TIMEOUT_INVALID",
            "judgeTimeoutMs must be a positive integer.",
            "router.tokenSaver.judgeTimeoutMs",
        );
    }
    let cache = field(raw, "cacheAwareSwitching");
    if shape(
        cache,
        d,
        "ROUTER_TOKEN_SAVER_CACHE_AWARE_SWITCHING_INVALID",
        "cacheAwareSwitching must be an object.",
        "router.tokenSaver.cacheAwareSwitching",
    ) {
        let n = field(cache, "minSavingsRatio");
        if !n.is_undefined() && !number(n, false, false) {
            d.fatal(
                "ROUTER_TOKEN_SAVER_CACHE_AWARE_SWITCHING_MIN_SAVINGS_INVALID",
                "cacheAwareSwitching.minSavingsRatio must be a non-negative number.",
                "router.tokenSaver.cacheAwareSwitching.minSavingsRatio",
            );
        }
    }
    names
}
fn auto_orchestrate(raw: &JsValue, tiers: &[JsString], d: &mut Diagnostics) {
    if !shape(
        raw,
        d,
        "ROUTER_AUTO_ORCHESTRATE_INVALID",
        "router.autoOrchestrate must be an object.",
        "router.autoOrchestrate",
    ) {
        return;
    }
    for key in ["mainAgentModel", "subagentModel"] {
        if !field(raw, key).is_undefined() {
            let path = format!("router.autoOrchestrate.{key}");
            d.warn(
                "ROUTER_AUTO_ORCHESTRATE_DEPRECATED_FIELD",
                format!("{path} is deprecated and ignored."),
                &path,
            );
        }
    }
    for (key, code, msg) in [
        (
            "triggerTiers",
            "ROUTER_AUTO_ORCHESTRATE_TRIGGER_TIERS_INVALID",
            "triggerTiers must be an array of strings.",
        ),
        (
            "allowedTools",
            "ROUTER_AUTO_ORCHESTRATE_ALLOWED_TOOLS_INVALID",
            "allowedTools must be an array of strings.",
        ),
        (
            "blockedTools",
            "ROUTER_AUTO_ORCHESTRATE_BLOCKED_TOOLS_INVALID",
            "blockedTools must be an array of strings.",
        ),
    ] {
        let v = field(raw, key);
        if !v.is_undefined() && !strings(v) {
            d.fatal(code, msg, &format!("router.autoOrchestrate.{key}"));
        } else if key == "triggerTiers"
            && !tiers.is_empty()
            && let JsValue::Array(a) = v
        {
            for t in a {
                if !tiers.contains(t.as_string().unwrap()) {
                    d.warn(
                        "ROUTER_AUTO_ORCHESTRATE_TRIGGER_TIER_UNKNOWN",
                        format!(
                            "Trigger tier {} not present in tokenSaver.tiers.",
                            js_string(t)
                        ),
                        "router.autoOrchestrate.triggerTiers",
                    );
                }
            }
        }
    }
    if strings(field(raw, "allowedTools")) && strings(field(raw, "blockedTools")) {
        d.warn(
            "ROUTER_AUTO_ORCHESTRATE_TOOLS_CONFLICT",
            "Both allowedTools and blockedTools are set; allowedTools takes precedence.",
            "router.autoOrchestrate",
        );
    }
    let n = field(raw, "subagentMaxTokens");
    if !n.is_undefined() && !number(n, true, true) {
        d.fatal(
            "ROUTER_AUTO_ORCHESTRATE_SUBAGENT_MAX_TOKENS_INVALID",
            "subagentMaxTokens must be a positive integer.",
            "router.autoOrchestrate.subagentMaxTokens",
        );
    }
}
fn stats(raw: &JsValue, p: &[Provider], d: &mut Diagnostics) {
    if !shape(
        raw,
        d,
        "ROUTER_STATS_INVALID",
        "router.stats must be an object.",
        "router.stats",
    ) {
        return;
    }
    bool_enabled(
        field(raw, "enabled"),
        d,
        "ROUTER_STATS_ENABLED_INVALID",
        "router.stats.enabled must be a boolean.",
        "router.stats.enabled",
    );
    let n = field(raw, "retentionMs");
    if !n.is_undefined()
        && (!number(n, true, true) || n.as_number().is_some_and(|n| n > 9007199254740991.0))
    {
        d.fatal(
            "ROUTER_STATS_RETENTION_INVALID",
            "router.stats.retentionMs must be a positive safe integer in milliseconds.",
            "router.stats.retentionMs",
        );
    }
    let pricing = field(raw, "modelPricing");
    if shape(
        pricing,
        d,
        "ROUTER_STATS_PRICING_INVALID",
        "modelPricing must be an object keyed by provider/model.",
        "router.stats.modelPricing",
    ) {
        for (key, body) in entries(pricing) {
            let path = format!(
                "router.stats.modelPricing.{}",
                js_string(&JsValue::String(key.clone()))
            );
            if !record(body) {
                d.fatal(
                    "ROUTER_STATS_PRICING_ENTRY_INVALID",
                    "modelPricing entries must be objects.",
                    &path,
                );
                continue;
            }
            if reference(&JsValue::String(key.clone()), &path, p, d).is_none() {
                continue;
            }
            for key in ["input", "output", "cacheRead"] {
                let v = field(body, key);
                if !v.is_undefined() && !number(v, false, false) {
                    d.fatal(
                        "ROUTER_STATS_PRICING_VALUE_INVALID",
                        "pricing values must be finite non-negative numbers.",
                        &format!("{path}.{key}"),
                    );
                }
            }
            let unit = field(body, "unit");
            if !unit.is_undefined() && !eq(unit, "$/百万 Token") && !eq(unit, "¥/百万 Token") {
                d.fatal(
                    "ROUTER_STATS_PRICING_UNIT_INVALID",
                    "unit must be one of $/百万 Token or ¥/百万 Token.",
                    &format!("{path}.unit"),
                );
            }
        }
    }
    let baseline = field(raw, "baselineModel");
    let path = "router.stats.baselineModel";
    if !baseline.is_undefined() {
        if baseline.as_string().is_some() {
            reference(baseline, path, p, d);
        } else if record(baseline)
            && field(baseline, "provider").as_string().is_some()
            && field(baseline, "model").as_string().is_some()
        {
            let a = trim(field(baseline, "provider").as_string().unwrap());
            let b = trim(field(baseline, "model").as_string().unwrap());
            if !a.is_empty() && !b.is_empty() {
                let mut units = a.units().to_vec();
                units.push(47);
                units.extend(b.units());
                reference(&JsValue::String(JsString::from_utf16(units)), path, p, d);
            } else {
                d.fatal(
                    "ROUTER_REF_INVALID",
                    format!("{path} must be an object with provider and model."),
                    path,
                );
            }
        } else {
            d.fatal(
                "ROUTER_REF_INVALID",
                format!("{path} must be an object with provider and model."),
                path,
            );
        }
    }
}
