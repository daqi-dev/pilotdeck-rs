//! Router configuration projection. The only accepted input is the JSON value
//! written by `config/snapshot.rs` (`snapshot.config.router`); the loader is
//! never re-run here, matching the A6.4-pr4 boundary.
use super::{field, number, object_field, truthy};
use crate::model_cost::PricingMap;
use pilotdeck_protocol::{JsString, JsValue};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouterModelRef {
    pub id: JsString,
    pub provider: JsString,
    pub model: JsString,
}
impl RouterModelRef {
    pub fn new(provider: JsString, model: JsString) -> Self {
        let mut units = provider.units().to_vec();
        units.push(47);
        units.extend_from_slice(model.units());
        Self {
            id: JsString::from_utf16(units),
            provider,
            model,
        }
    }
    pub fn from_value(value: &JsValue) -> Option<Self> {
        let provider = field(value, "provider").as_string()?.clone();
        let model = field(value, "model").as_string()?.clone();
        if !truthy(&JsValue::String(provider.clone())) || !truthy(&JsValue::String(model.clone())) {
            return None;
        }
        Some(Self::new(provider, model))
    }
    pub fn key(&self) -> (JsString, JsString) {
        (self.provider.clone(), self.model.clone())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SubagentPolicy {
    Skip,
    #[default]
    Judge,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TierConfig {
    pub model: RouterModelRef,
    pub description: Option<JsString>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CacheAwareSwitching {
    pub enabled: bool,
    pub min_savings_ratio: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TokenSaverConfig {
    pub enabled: bool,
    pub judge: RouterModelRef,
    pub default_tier: JsString,
    /// `Object.keys(config.tiers)` order: snapshot already applied JS ordering.
    pub tiers: Vec<(JsString, TierConfig)>,
    pub rules: Vec<JsString>,
    pub subagent_policy: SubagentPolicy,
    pub judge_timeout_ms: f64,
    pub cache_aware: CacheAwareSwitching,
}
impl TokenSaverConfig {
    pub fn tier(&self, name: &JsString) -> Option<&TierConfig> {
        self.tiers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, tier)| tier)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FallbackConfig {
    /// Insertion order of `fallback` keys, excluding `maxFallbacks`.
    pub by_scenario: Vec<(String, Vec<RouterModelRef>)>,
    pub default: Vec<RouterModelRef>,
    pub max_fallbacks: Option<f64>,
}
impl FallbackConfig {
    pub fn candidates(&self, scenario: &str) -> Vec<RouterModelRef> {
        let mut out: Vec<RouterModelRef> = Vec::new();
        let mut add = |refs: &[RouterModelRef]| {
            for reference in refs {
                if !out
                    .iter()
                    .any(|candidate| candidate.key() == reference.key())
                {
                    out.push(reference.clone());
                }
            }
        };
        if let Some((_, refs)) = self.by_scenario.iter().find(|(name, _)| name == scenario) {
            add(refs);
        }
        add(&self.default);
        out
    }

    pub fn plan(&self, scenario: &str) -> Vec<RouterModelRef> {
        if scenario == "explicit" {
            return Vec::new();
        }
        let attempts = self
            .by_scenario
            .iter()
            .find(|(name, _)| name == scenario)
            .map(|(_, refs)| refs)
            .unwrap_or(&self.default);
        let cap = self.max_fallbacks.unwrap_or(5.0);
        if cap <= 0.0 || !cap.is_finite() {
            return Vec::new();
        }
        let cap = (cap as usize).min(attempts.len());
        attempts[..cap].to_vec()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ZeroUsageRetry {
    pub enabled: bool,
    pub max_attempts: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransientRetry {
    pub enabled: bool,
    pub max_attempts: f64,
    pub base_delay_ms: f64,
    pub max_delay_ms: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AutoOrchestrateConfig {
    pub enabled: bool,
    pub trigger_tiers: Vec<JsString>,
    pub subagent_max_tokens: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatsConfig {
    pub enabled: bool,
    pub model_pricing: Option<PricingMap>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RouterConfig {
    pub enabled: bool,
    pub scenarios_default: Option<RouterModelRef>,
    pub fallback: FallbackConfig,
    pub zero_usage_retry: ZeroUsageRetry,
    pub transient_retry: TransientRetry,
    pub token_saver: Option<TokenSaverConfig>,
    pub auto_orchestrate: Option<AutoOrchestrateConfig>,
    pub stats: StatsConfig,
    pub custom_router: Option<JsString>,
    /// Every configured `scenarios.*` key, in snapshot order (used by the
    /// custom-router context; only `default` has a model).
    pub scenario_keys: Vec<JsString>,
}
impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scenarios_default: None,
            fallback: FallbackConfig::default(),
            zero_usage_retry: ZeroUsageRetry {
                enabled: true,
                max_attempts: 5.0,
            },
            transient_retry: TransientRetry {
                enabled: true,
                max_attempts: 2.0,
                base_delay_ms: 500.0,
                max_delay_ms: 8_000.0,
            },
            token_saver: None,
            auto_orchestrate: None,
            stats: StatsConfig::default(),
            custom_router: None,
            scenario_keys: Vec::new(),
        }
    }
}
impl RouterConfig {
    /// Read `snapshot.config.router` from the already-built configuration
    /// snapshot. A snapshot without a router section means "router defaults".
    pub fn from_snapshot(snapshot: &JsValue) -> Self {
        let Some(router) = snapshot
            .get("config")
            .and_then(|config| config.get("router"))
        else {
            return Self::default();
        };
        Self::from_value(router)
    }

    fn from_value(router: &JsValue) -> Self {
        let mut config = Self::default();
        let Some(object) = router.as_object() else {
            return config;
        };
        config.enabled = object
            .get("enabled")
            .and_then(JsValue::as_bool)
            .unwrap_or(true);
        if let Some(scenarios) = object_field(router, "scenarios") {
            config.scenario_keys = scenarios
                .entries()
                .iter()
                .map(|(key, _)| key.clone())
                .collect();
            config.scenarios_default = scenarios
                .get("default")
                .and_then(RouterModelRef::from_value);
        }
        config.fallback = parse_fallback(field(router, "fallback"));
        config.zero_usage_retry = parse_zero_usage(field(router, "zeroUsageRetry"));
        config.transient_retry = parse_transient(field(router, "transientRetry"));
        config.token_saver = parse_token_saver(field(router, "tokenSaver"));
        config.auto_orchestrate = parse_auto_orchestrate(field(router, "autoOrchestrate"));
        config.stats = parse_stats(field(router, "stats"));
        config.custom_router = field(router, "customRouter")
            .get("extensionId")
            .and_then(JsValue::as_string)
            .cloned();
        config
    }

    pub fn scenario_names(&self) -> Vec<JsString> {
        self.scenario_keys.clone()
    }

    pub fn token_saver(&self) -> Option<&TokenSaverConfig> {
        self.token_saver.as_ref()
    }
}

fn parse_fallback(value: &JsValue) -> FallbackConfig {
    let mut fallback = FallbackConfig::default();
    let Some(object) = value.as_object() else {
        return fallback;
    };
    for (key, body) in object.entries() {
        let name = String::from_utf16_lossy(key.units());
        if name == "maxFallbacks" {
            fallback.max_fallbacks = body.as_number().filter(|value| value.is_finite());
            continue;
        }
        let refs = parse_refs(body);
        if name == "default" {
            fallback.default = refs;
        } else {
            fallback.by_scenario.push((name, refs));
        }
    }
    fallback
}
fn parse_refs(value: &JsValue) -> Vec<RouterModelRef> {
    match value {
        JsValue::Array(items) => items
            .iter()
            .filter_map(RouterModelRef::from_value)
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_zero_usage(value: &JsValue) -> ZeroUsageRetry {
    ZeroUsageRetry {
        enabled: field(value, "enabled").as_bool().unwrap_or(true),
        max_attempts: number(field(value, "maxAttempts")).unwrap_or(5.0).max(1.0),
    }
}
fn parse_transient(value: &JsValue) -> TransientRetry {
    TransientRetry {
        enabled: field(value, "enabled").as_bool().unwrap_or(true),
        max_attempts: number(field(value, "maxAttempts")).unwrap_or(2.0).max(1.0),
        base_delay_ms: number(field(value, "baseDelayMs")).unwrap_or(500.0),
        max_delay_ms: number(field(value, "maxDelayMs")).unwrap_or(8_000.0),
    }
}

fn parse_token_saver(value: &JsValue) -> Option<TokenSaverConfig> {
    let object = value.as_object()?;
    let enabled = object
        .get("enabled")
        .and_then(JsValue::as_bool)
        .unwrap_or(true);
    let judge = RouterModelRef::from_value(field(value, "judge"))?;
    let tiers_object = object_field(value, "tiers")?;
    let mut tiers = Vec::new();
    for (name, body) in tiers_object.entries() {
        let Some(model) = RouterModelRef::from_value(field(body, "model")) else {
            continue;
        };
        tiers.push((
            name.clone(),
            TierConfig {
                model,
                description: field(body, "description").as_string().cloned(),
            },
        ));
    }
    if tiers.is_empty() {
        return None;
    }
    let default_tier = object
        .get("defaultTier")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    let rules = match object.get("rules") {
        Some(JsValue::Array(items)) => items
            .iter()
            .filter_map(JsValue::as_string)
            .cloned()
            .collect(),
        _ => Vec::new(),
    };
    let subagent_policy = match object
        .get("subagent")
        .and_then(|subagent| subagent.get("policy"))
        .and_then(JsValue::as_string)
        .map(|policy| policy.to_utf8().unwrap_or_default())
        .as_deref()
    {
        Some("skip") => SubagentPolicy::Skip,
        _ => SubagentPolicy::Judge,
    };
    let judge_timeout_ms = object
        .get("judgeTimeoutMs")
        .and_then(JsValue::as_number)
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(5_000.0);
    let cache_aware = object
        .get("cacheAwareSwitching")
        .and_then(JsValue::as_object)
        .map(|cache| CacheAwareSwitching {
            enabled: cache
                .get("enabled")
                .and_then(JsValue::as_bool)
                .unwrap_or(true),
            min_savings_ratio: cache
                .get("minSavingsRatio")
                .and_then(JsValue::as_number)
                .filter(|value| value.is_finite() && *value >= 0.0)
                .unwrap_or(0.0),
        })
        .unwrap_or(CacheAwareSwitching {
            enabled: true,
            min_savings_ratio: 0.0,
        });
    Some(TokenSaverConfig {
        enabled,
        judge,
        default_tier,
        tiers,
        rules,
        subagent_policy,
        judge_timeout_ms,
        cache_aware,
    })
}

fn parse_auto_orchestrate(value: &JsValue) -> Option<AutoOrchestrateConfig> {
    let object = value.as_object()?;
    let trigger_tiers = match object.get("triggerTiers") {
        Some(JsValue::Array(items)) => items
            .iter()
            .filter_map(JsValue::as_string)
            .cloned()
            .collect(),
        _ => Vec::new(),
    };
    Some(AutoOrchestrateConfig {
        enabled: object
            .get("enabled")
            .and_then(JsValue::as_bool)
            .unwrap_or(true),
        trigger_tiers,
        subagent_max_tokens: object
            .get("subagentMaxTokens")
            .and_then(JsValue::as_number)
            .filter(|value| value.is_finite() && *value > 0.0),
    })
}

fn parse_stats(value: &JsValue) -> StatsConfig {
    let Some(object) = value.as_object() else {
        return StatsConfig::default();
    };
    let enabled = object
        .get("enabled")
        .and_then(JsValue::as_bool)
        .unwrap_or(true);
    let model_pricing = object
        .get("modelPricing")
        .and_then(JsValue::as_object)
        .map(|pricing| {
            let mut map = PricingMap::new();
            for (key, body) in pricing.entries() {
                let Some(body) = body.as_object() else {
                    continue;
                };
                let rate = |field: &str| body.get(field).and_then(JsValue::as_number);
                map.insert(
                    String::from_utf16_lossy(key.units()),
                    crate::model_cost::ModelPricing {
                        input: rate("input"),
                        output: rate("output"),
                        cache_read: rate("cacheRead"),
                    },
                );
            }
            map
        });
    StatsConfig {
        enabled,
        model_pricing,
    }
}

/// Runtime defaults used by `RouterRetryPolicy`; exported for replay tests.
pub const LITELLM_DEFAULT_MAX_RETRIES: f64 = 2.0;
pub const LITELLM_INITIAL_RETRY_DELAY_MS: f64 = 500.0;
pub const LITELLM_MAX_RETRY_DELAY_MS: f64 = 8_000.0;
pub const LITELLM_RETRY_JITTER: f64 = 0.75;
