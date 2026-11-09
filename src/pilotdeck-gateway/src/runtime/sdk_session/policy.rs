use super::{
    SdkSessionError, array, field, math_min, number, number_text, own, set_property, slash,
    trim_units, truthy,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use regex::Regex;
use std::{collections::HashSet, sync::OnceLock};

// ─────────────────────────────────────────────────────────── model resolution

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRef {
    pub provider: JsString,
    pub model: JsString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelField {
    Model,
    AgentModel,
    FallbackModel,
    SubagentModel,
}

impl ModelField {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::Model => "INVALID_SDK_MODEL",
            Self::AgentModel => "INVALID_SDK_AGENT_MODEL",
            Self::FallbackModel => "INVALID_SDK_FALLBACK_MODEL",
            Self::SubagentModel => "INVALID_SDK_SUBAGENT_MODEL",
        }
    }
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::AgentModel => "settings.agent.model",
            Self::FallbackModel => "fallbackModel",
            Self::SubagentModel => "settings.agent.subagents.default",
        }
    }
}

/// `resolveSdkConfiguredModel`: exactly one catalog model must match the id,
/// the `provider/model` reference, or the model key.
pub fn resolve_configured_model(
    snapshot: &JsValue,
    requested: &JsString,
    model_field: ModelField,
) -> Result<ModelRef, SdkSessionError> {
    let providers = field(field(snapshot, "config"), "model").get("providers");
    let mut candidates: Vec<ModelRef> = Vec::new();
    if let Some(providers) = providers.and_then(JsValue::as_object) {
        for (provider_id, provider) in providers.entries() {
            let Some(models) = provider.get("models").and_then(JsValue::as_object) else {
                continue;
            };
            for (model_id, definition) in models.entries() {
                let id_matches = definition
                    .get("id")
                    .and_then(JsValue::as_string)
                    .is_some_and(|id| id == requested);
                let reference = slash(&[provider_id, model_id]);
                if id_matches || &reference == requested || model_id == requested {
                    candidates.push(ModelRef {
                        provider: provider_id.clone(),
                        model: model_id.clone(),
                    });
                }
            }
        }
    }
    if candidates.len() == 1 {
        return Ok(candidates.remove(0));
    }
    Err(SdkSessionError::new(
        model_field.code(),
        format!(
            "{} is not uniquely resolvable from the Gateway catalog: {}",
            model_field.label(),
            number_text(requested)
        ),
    ))
}

fn selector_matches(selector: &str, model: &ModelRef) -> bool {
    if selector == "*" {
        return true;
    }
    let provider = String::from_utf16_lossy(model.provider.units());
    let name = String::from_utf16_lossy(model.model.units());
    selector == format!("{provider}/*") || selector == format!("{provider}/{name}")
}

/// `assertSdkManagedModelAllowed` against the raw `managedModels` value.
pub fn assert_managed_model_allowed(
    policy: Option<&JsValue>,
    model: &ModelRef,
) -> Result<(), SdkSessionError> {
    let Some(policy) = policy else {
        return Ok(());
    };
    if !truthy(policy) {
        return Ok(());
    }
    let Some(policy) = policy.as_object() else {
        return Ok(());
    };
    let strings = |key: &str| -> Vec<String> {
        policy
            .get(key)
            .and_then(|value| array(value))
            .map(|items| {
                items
                    .iter()
                    .filter_map(JsValue::as_string)
                    .map(number_text)
                    .collect()
            })
            .unwrap_or_default()
    };
    let deny = strings("deny");
    let allow = strings("allow");
    if !deny
        .iter()
        .any(|selector| selector_matches(selector, model))
        && (allow.is_empty()
            || allow
                .iter()
                .any(|selector| selector_matches(selector, model)))
    {
        return Ok(());
    }
    let provider = String::from_utf16_lossy(model.provider.units());
    let name = String::from_utf16_lossy(model.model.units());
    Err(SdkSessionError::new(
        "SDK_MANAGED_MODEL_DENIED",
        format!("SDK managedSettings.models denies model {provider}/{name}."),
    ))
}

pub fn is_organization_model_allowed(policy: Option<&SelectorPolicy>, model: &ModelRef) -> bool {
    let Some(policy) = policy else {
        return true;
    };
    !policy
        .deny
        .iter()
        .any(|selector| selector_matches(selector, model))
        && (policy.allow.is_empty()
            || policy
                .allow
                .iter()
                .any(|selector| selector_matches(selector, model)))
}

fn url_origin(value: &JsValue) -> Option<String> {
    if !truthy(value) {
        return None;
    }
    let text = value.as_string()?;
    url::Url::parse(&String::from_utf16_lossy(text.units()))
        .ok()
        .map(|url| url.origin().ascii_serialization())
}

fn contains(values: &[String], value: &str) -> bool {
    values.iter().any(|candidate| candidate == value)
}

pub fn organization_provider_policy_rejection(
    policy: Option<&ProviderPolicy>,
    provider_id: &JsString,
    provider: Option<&JsValue>,
) -> Option<SdkSessionError> {
    let policy = policy?;
    let provider_id_text = String::from_utf16_lossy(provider_id.units());
    if contains(&policy.deny, &provider_id_text)
        || (!policy.allow.is_empty() && !contains(&policy.allow, &provider_id_text))
    {
        return Some(SdkSessionError::new(
            "GATEWAY_ORGANIZATION_PROVIDER_DENIED",
            format!("Gateway organization policy denies provider {provider_id_text}."),
        ));
    }
    let origin = provider
        .and_then(|provider| provider.get("url"))
        .and_then(url_origin);
    let origin_key = origin.clone().unwrap_or_default();
    if contains(&policy.origins.deny, &origin_key)
        || (!policy.origins.allow.is_empty() && !contains(&policy.origins.allow, &origin_key))
    {
        return Some(SdkSessionError::new(
            "GATEWAY_ORGANIZATION_PROVIDER_ORIGIN_DENIED",
            format!(
                "Gateway organization policy denies provider origin {} for {provider_id_text}.",
                origin.unwrap_or_else(|| "<unknown>".into())
            ),
        ));
    }
    let credential_source = provider
        .and_then(|provider| provider.get("credentialSource"))
        .filter(|value| truthy(value))
        .and_then(JsValue::as_string)
        .map(number_text);
    if let Some(credential_source) = &credential_source {
        if contains(&policy.credentials.deny, credential_source)
            || (!policy.credentials.allow.is_empty()
                && !contains(&policy.credentials.allow, credential_source))
        {
            return Some(SdkSessionError::new(
                "GATEWAY_ORGANIZATION_CREDENTIAL_SOURCE_DENIED",
                format!(
                    "Gateway organization policy denies {credential_source} credentials for provider {provider_id_text}."
                ),
            ));
        }
    } else if !policy.credentials.allow.is_empty() {
        return Some(SdkSessionError::new(
            "GATEWAY_ORGANIZATION_CREDENTIAL_SOURCE_DENIED",
            format!(
                "Gateway organization policy requires a known credential source for provider {provider_id_text}."
            ),
        ));
    }
    None
}

// ─────────────────────────────────────────────────────────── validators

fn invalid_managed(kind: &str, label: &str) -> SdkSessionError {
    SdkSessionError::new(
        "INVALID_SDK_MANAGED_SETTINGS",
        format!("{kind}.{label} contains an invalid selector."),
    )
}

fn duplicate_managed(kind: &str, label: &str) -> SdkSessionError {
    SdkSessionError::new(
        "INVALID_SDK_MANAGED_SETTINGS",
        format!("{kind}.{label} cannot contain duplicates."),
    )
}

fn tool_selector() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_.:-]*\*?$").unwrap())
}

fn model_selector() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"^[^/{}]+/(?:[^/{}]+|\*)$",
            crate::metadata::SPACE,
            crate::metadata::SPACE
        ))
        .unwrap()
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectorKind {
    Tools,
    Models,
}

impl SelectorKind {
    fn label(self) -> &'static str {
        match self {
            Self::Tools => "managedTools",
            Self::Models => "managedModels",
        }
    }
}

/// `validateSdkManagedPermissions`.
pub fn validate_managed_permissions(value: Option<&JsValue>) -> Result<(), SdkSessionError> {
    let Some(value) = value else {
        return Ok(());
    };
    if !truthy(value) {
        return Ok(());
    }
    for label in ["deny", "ask"] {
        let Some(entries) = array(field(value, label)) else {
            return Err(SdkSessionError::new(
                "INVALID_SDK_MANAGED_SETTINGS",
                format!("managedPermissions.{label} must contain non-empty strings."),
            ));
        };
        for entry in entries {
            let Some(text) = entry.as_string() else {
                return Err(SdkSessionError::new(
                    "INVALID_SDK_MANAGED_SETTINGS",
                    format!("managedPermissions.{label} must contain non-empty strings."),
                ));
            };
            if trim_units(text).is_empty() {
                return Err(SdkSessionError::new(
                    "INVALID_SDK_MANAGED_SETTINGS",
                    format!("managedPermissions.{label} must contain non-empty strings."),
                ));
            }
        }
    }
    if let Some(mode) = own(value, "defaultMode")
        && !mode.is_undefined()
        && !mode.as_string().is_some_and(|mode| mode.equals_str("plan"))
    {
        return Err(SdkSessionError::new(
            "UNSUPPORTED_SDK_MANAGED_SETTINGS",
            "managedPermissions.defaultMode may only force plan mode.",
        ));
    }
    if let Some(can_prompt) = own(value, "canPrompt")
        && !can_prompt.is_undefined()
        && can_prompt.as_bool() != Some(false)
    {
        return Err(SdkSessionError::new(
            "UNSUPPORTED_SDK_MANAGED_SETTINGS",
            "managedPermissions.canPrompt may only disable prompts.",
        ));
    }
    Ok(())
}

/// `validateSdkSelectorPolicy`.
pub fn validate_selector_policy(
    value: Option<&JsValue>,
    kind: SelectorKind,
) -> Result<(), SdkSessionError> {
    let Some(value) = value else {
        return Ok(());
    };
    if !truthy(value) {
        return Ok(());
    }
    let regex = match kind {
        SelectorKind::Tools => tool_selector(),
        SelectorKind::Models => model_selector(),
    };
    for label in ["allow", "deny"] {
        let Some(entries) = array(field(value, label)) else {
            return Err(invalid_managed(kind.label(), label));
        };
        let mut seen: HashSet<JsString> = HashSet::new();
        for entry in entries {
            let Some(text) = entry.as_string() else {
                return Err(invalid_managed(kind.label(), label));
            };
            let trimmed = trim_units(text);
            if !trimmed.equals_str("*")
                && !regex.is_match(&String::from_utf16_lossy(trimmed.units()))
            {
                return Err(invalid_managed(kind.label(), label));
            }
            if !seen.insert(trimmed) {
                return Err(duplicate_managed(kind.label(), label));
            }
        }
    }
    Ok(())
}

/// `applyOrganizationTaskBudgetCap`. Mutates the cloned session config.
pub fn apply_organization_task_budget_cap(config: &mut JsValue, cap: Option<f64>) {
    let Some(cap) = cap else {
        return;
    };
    if !matches!(config, JsValue::Object(_)) {
        return;
    }
    let budget = field(config, "taskBudget").clone();
    let total = if truthy(&budget) {
        math_min(number(field(&budget, "total")), cap)
    } else {
        cap
    };
    let mut next = JsObject::new();
    next.insert("total".into(), JsValue::Number(total));
    if field(&budget, "scope")
        .as_string()
        .is_some_and(|scope| scope.equals_str("project"))
    {
        next.insert("scope".into(), "project".into());
    }
    if let Some(retention) =
        own(&budget, "projectRetentionMs").filter(|value| !value.is_undefined())
    {
        next.insert("projectRetentionMs".into(), retention.clone());
    }
    set_property(config, "taskBudget", JsValue::Object(next));
}

/// `assertOrganizationSettingSourcesAllowed`.
pub fn assert_organization_setting_sources_allowed(
    policy: Option<&SelectorPolicy>,
    sources: Option<&JsValue>,
) -> Result<(), SdkSessionError> {
    let Some(policy) = policy else {
        return Ok(());
    };
    let Some(sources) = sources else {
        return Ok(());
    };
    let Some(items) = array(sources) else {
        return Ok(());
    };
    for source in items {
        let Some(source) = source.as_string() else {
            continue;
        };
        let text = number_text(source);
        if contains(&policy.deny, &text)
            || (!policy.allow.is_empty() && !contains(&policy.allow, &text))
        {
            return Err(SdkSessionError::new(
                "GATEWAY_ORGANIZATION_SETTING_SOURCE_DENIED",
                format!("Gateway organization policy denies the {text} SDK settings source."),
            ));
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────── organization policy

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectorPolicy {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderPolicy {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub origins: SelectorPolicy,
    pub credentials: SelectorPolicy,
}

#[derive(Clone, Debug, Default)]
pub struct OrganizationSettingsPolicy {
    pub can_update_local_settings: Option<bool>,
    pub max_context_tokens: Option<f64>,
    pub max_output_tokens: Option<f64>,
    pub max_thinking_tokens: Option<f64>,
    pub max_subagent_timeout_ms: Option<f64>,
    pub session_defaults: Option<JsValue>,
    pub managed_session_settings: Option<JsValue>,
    pub session_default_sources: Option<JsValue>,
    pub enforced_session_settings: Option<JsValue>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OrganizationPermissionsPolicy {
    pub deny: Vec<String>,
    pub ask: Vec<String>,
    pub default_plan: bool,
    pub can_prompt: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct OrganizationPolicy {
    /// `organizationPolicy.permissions`; only restrictive values are accepted.
    pub permissions: Option<OrganizationPermissionsPolicy>,
    pub models: Option<SelectorPolicy>,
    pub providers: Option<ProviderPolicy>,
    /// `organizationPolicy.tools`; the SDK tool selector.
    pub tools: Option<SelectorPolicy>,
    pub setting_sources: Option<SelectorPolicy>,
    pub settings: Option<OrganizationSettingsPolicy>,
    pub max_task_budget_usd: Option<f64>,
    /// `organizationPolicy.limits.maxBudgetUsd`; a positive finite number.
    pub max_budget_usd: Option<f64>,
    /// `organizationPolicy.limits.maxTurns`; a positive safe integer.
    pub max_turns: Option<f64>,
    /// `organizationPolicy.limits.maxSubagentDepth`; a non-negative integer.
    pub max_subagent_depth: Option<f64>,
    /// `organizationPolicy.settings.maxSubagentTimeoutMs`.
    pub max_subagent_timeout_ms: Option<f64>,
}

impl OrganizationPolicy {
    /// `hasOrganizationSdkSessionDefaults`.
    pub fn has_session_defaults(&self) -> bool {
        self.settings.as_ref().is_some_and(|settings| {
            settings.session_defaults.is_some()
                || settings.managed_session_settings.is_some()
                || settings.session_default_sources.is_some()
                || settings.enforced_session_settings.is_some()
        }) || self.max_task_budget_usd.is_some()
    }
    pub fn permission_rules(&self) -> crate::config::permission::PermissionRuleSet {
        use crate::config::permission::PermissionRuleSet;
        let Some(policy) = &self.permissions else {
            return PermissionRuleSet::default();
        };
        PermissionRuleSet {
            allow: Vec::new(),
            deny: policy
                .deny
                .iter()
                .map(|entry| organization_permission_rule(entry, "deny"))
                .collect(),
            ask: policy
                .ask
                .iter()
                .map(|entry| organization_permission_rule(entry, "ask"))
                .collect(),
        }
    }

    pub fn tool_policy_value(&self) -> Option<JsValue> {
        let policy = self.tools.as_ref()?;
        let array = |values: &[String]| {
            JsValue::Array(
                values
                    .iter()
                    .map(|value| JsValue::String(value.as_str().into()))
                    .collect(),
            )
        };
        let mut value = JsObject::new();
        value.insert("allow".into(), array(&policy.allow));
        value.insert("deny".into(), array(&policy.deny));
        Some(JsValue::Object(value))
    }

    pub fn apply_agent_runtime_caps(&self, agent: &mut JsObject) {
        let Some(settings) = &self.settings else {
            return;
        };
        for (key, cap) in [
            ("maxContextTokens", settings.max_context_tokens),
            ("maxOutputTokens", settings.max_output_tokens),
        ] {
            let Some(cap) = cap else { continue };
            let value = agent
                .get(key)
                .and_then(JsValue::as_number)
                .map_or(cap, |value| value.min(cap));
            agent.insert(key.into(), JsValue::Number(value));
        }
        let Some(cap) = settings.max_thinking_tokens else {
            return;
        };
        let Some(mut thinking) = agent.get("thinking").and_then(JsValue::as_object).cloned() else {
            return;
        };
        if thinking.get("enabled").and_then(JsValue::as_bool) != Some(true) {
            return;
        }
        if cap == 0.0 {
            let mut disabled = JsObject::new();
            disabled.insert("enabled".into(), JsValue::Bool(false));
            disabled.insert("mode".into(), "off".into());
            agent.insert("thinking".into(), JsValue::Object(disabled));
            return;
        }
        let budget = thinking
            .get("budgetTokens")
            .and_then(JsValue::as_number)
            .map_or(cap, |value| value.min(cap));
        thinking.insert("budgetTokens".into(), JsValue::Number(budget));
        agent.insert("thinking".into(), JsValue::Object(thinking));
    }
}

fn js_trim(value: &str) -> &str {
    value.trim_matches(|char| {
        matches!(
            char,
            '\u{0009}'
                | '\u{000a}'
                | '\u{000b}'
                | '\u{000c}'
                | '\u{000d}'
                | '\u{0020}'
                | '\u{00a0}'
                | '\u{1680}'
                | '\u{2000}'
                ..='\u{200a}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202f}'
                    | '\u{205f}'
                    | '\u{3000}'
                    | '\u{feff}'
        )
    })
}

fn normalize_permission_entry(entry: &str) -> String {
    let trimmed = js_trim(entry);
    if let Some(pattern) = trimmed
        .strip_prefix("Bash(")
        .and_then(|value| value.strip_suffix(')'))
        .filter(|value| {
            !value
                .chars()
                .any(|char| matches!(char, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
        })
    {
        let pattern = js_trim(pattern);
        return if pattern.is_empty() {
            "bash".to_owned()
        } else {
            format!("bash:{pattern}")
        };
    }
    match trimmed {
        "Read" => "read_file".to_owned(),
        "Write" => "write_file".to_owned(),
        "Edit" | "MultiEdit" => "edit_file".to_owned(),
        "NotebookEdit" => "edit_notebook".to_owned(),
        "Glob" => "glob".to_owned(),
        "Grep" => "grep".to_owned(),
        "Bash" => "bash".to_owned(),
        "Task" => "agent".to_owned(),
        "TodoWrite" => "todo_write".to_owned(),
        "WebFetch" => "web_fetch".to_owned(),
        "WebSearch" => "web_search".to_owned(),
        _ => trimmed.to_owned(),
    }
}

pub(crate) fn organization_permission_rule(
    entry: &str,
    behavior: &str,
) -> crate::config::permission::PermissionRule {
    let normalized = normalize_permission_entry(entry);
    let mut parts = normalized.split(':');
    let tool = parts.next().unwrap_or(&normalized);
    let pattern = parts.collect::<Vec<_>>().join(":");
    let pattern = js_trim(&pattern);
    let rule = crate::config::permission::PermissionRule::new(
        "policy",
        behavior,
        if tool.is_empty() { &normalized } else { tool },
    );
    if pattern.is_empty() {
        rule
    } else {
        rule.with_pattern(pattern)
    }
}

impl OrganizationPolicy {
    pub fn apply_execution_limits_json(
        &mut self,
        value: &serde_json::Value,
    ) -> Result<(), SdkSessionError> {
        let policy = organization_object(value, "organizationPolicy must be an object.")?;
        assert_organization_keys(
            policy,
            &[
                "permissions",
                "models",
                "providers",
                "tools",
                "settingSources",
                "limits",
                "settings",
            ],
        )?;
        self.permissions = normalize_organization_permissions(value.get("permissions"))?;
        self.models = normalize_organization_selector(value.get("models"), SelectorKind::Models)?;
        self.tools = normalize_organization_selector(value.get("tools"), SelectorKind::Tools)?;
        self.providers = normalize_organization_providers(value.get("providers"))?;
        self.setting_sources = normalize_organization_setting_sources(value.get("settingSources"))?;
        let limits = match value.get("limits") {
            Some(value) => {
                let limits =
                    organization_object(value, "organizationPolicy.limits must be an object.")?;
                assert_organization_keys(
                    limits,
                    &[
                        "maxTurns",
                        "maxBudgetUsd",
                        "maxTaskBudgetUsd",
                        "maxSubagentDepth",
                    ],
                )?;
                Some(value)
            }
            None => None,
        };
        let settings = match value.get("settings") {
            Some(value) => {
                let settings =
                    organization_object(value, "organizationPolicy.settings must be an object.")?;
                assert_organization_keys(
                    settings,
                    &[
                        "canUpdateLocalSettings",
                        "maxContextTokens",
                        "maxOutputTokens",
                        "maxThinkingTokens",
                        "maxSubagentTimeoutMs",
                        "sessionDefaults",
                        "managedSessionSettings",
                        "sessionDefaultSources",
                        "enforcedSessionSettings",
                    ],
                )?;
                Some(value)
            }
            None => None,
        };
        self.max_turns = optional_safe_integer(
            limits.and_then(|limits| limits.get("maxTurns")),
            false,
            "organizationPolicy.limits.maxTurns must be a positive safe integer.",
        )?;
        self.max_subagent_depth = optional_safe_integer(
            limits.and_then(|limits| limits.get("maxSubagentDepth")),
            true,
            "organizationPolicy.limits.maxSubagentDepth must be a non-negative safe integer.",
        )?;
        self.max_budget_usd = optional_positive_finite(
            limits.and_then(|limits| limits.get("maxBudgetUsd")),
            "organizationPolicy.limits.maxBudgetUsd must be a positive finite number.",
        )?;
        self.max_task_budget_usd = optional_positive_finite(
            limits.and_then(|limits| limits.get("maxTaskBudgetUsd")),
            "organizationPolicy.limits.maxTaskBudgetUsd must be a positive finite number.",
        )?;
        self.max_subagent_timeout_ms = optional_safe_integer(
            settings.and_then(|settings| settings.get("maxSubagentTimeoutMs")),
            false,
            "organizationPolicy.settings.maxSubagentTimeoutMs must be a positive safe integer.",
        )?;
        if settings
            .and_then(|settings| settings.get("canUpdateLocalSettings"))
            .is_some_and(|value| value != &serde_json::Value::Bool(false))
        {
            return Err(SdkSessionError::new(
                "UNSUPPORTED_GATEWAY_ORGANIZATION_POLICY",
                "organizationPolicy.settings.canUpdateLocalSettings may only disable SDK settings updates.",
            ));
        }
        let max_context_tokens = optional_safe_integer(
            settings.and_then(|settings| settings.get("maxContextTokens")),
            false,
            "organizationPolicy.settings.maxContextTokens must be a positive safe integer.",
        )?;
        let max_output_tokens = optional_safe_integer(
            settings.and_then(|settings| settings.get("maxOutputTokens")),
            false,
            "organizationPolicy.settings.maxOutputTokens must be a positive safe integer.",
        )?;
        let max_thinking_tokens = optional_safe_integer(
            settings.and_then(|settings| settings.get("maxThinkingTokens")),
            true,
            "organizationPolicy.settings.maxThinkingTokens must be a non-negative safe integer.",
        )?;
        let session_defaults = normalize_organization_session_settings(
            settings.and_then(|settings| settings.get("sessionDefaults")),
        )?;
        let managed_session_settings = normalize_organization_session_settings(
            settings.and_then(|settings| settings.get("managedSessionSettings")),
        )?;
        let enforced_session_settings = normalize_organization_session_settings(
            settings.and_then(|settings| settings.get("enforcedSessionSettings")),
        )?;
        let session_default_sources = normalize_organization_session_sources(
            settings.and_then(|settings| settings.get("sessionDefaultSources")),
        )?;
        let can_update_local_settings = settings
            .and_then(|settings| settings.get("canUpdateLocalSettings"))
            .map(|_| false);
        let organization_settings = OrganizationSettingsPolicy {
            can_update_local_settings,
            max_context_tokens,
            max_output_tokens,
            max_thinking_tokens,
            max_subagent_timeout_ms: self.max_subagent_timeout_ms,
            session_defaults,
            managed_session_settings,
            session_default_sources,
            enforced_session_settings,
        };
        self.settings = organization_settings_has_values(&organization_settings)
            .then_some(organization_settings);
        Ok(())
    }
}

fn invalid_organization(message: impl Into<String>) -> SdkSessionError {
    SdkSessionError::new("INVALID_GATEWAY_ORGANIZATION_POLICY", message)
}

fn unsupported_organization(message: impl Into<String>) -> SdkSessionError {
    SdkSessionError::new("UNSUPPORTED_GATEWAY_ORGANIZATION_POLICY", message)
}

fn organization_settings_has_values(settings: &OrganizationSettingsPolicy) -> bool {
    settings.can_update_local_settings.is_some()
        || settings.max_context_tokens.is_some()
        || settings.max_output_tokens.is_some()
        || settings.max_thinking_tokens.is_some()
        || settings.max_subagent_timeout_ms.is_some()
        || settings.session_defaults.is_some()
        || settings.managed_session_settings.is_some()
        || settings.session_default_sources.is_some()
        || settings.enforced_session_settings.is_some()
}

fn organization_string_array(
    value: Option<&serde_json::Value>,
    label: &str,
) -> Result<Vec<String>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(invalid_organization(format!(
            "{label} must be an array of non-empty strings."
        )));
    };
    let mut result = Vec::with_capacity(items.len());
    for item in items {
        let Some(item) = item.as_str() else {
            return Err(invalid_organization(format!(
                "{label} must be an array of non-empty strings."
            )));
        };
        let item = js_trim(item);
        if item.is_empty() {
            return Err(invalid_organization(format!(
                "{label} must be an array of non-empty strings."
            )));
        }
        if result.iter().any(|existing| existing == item) {
            return Err(invalid_organization(format!(
                "{label} cannot contain duplicate entries."
            )));
        }
        result.push(item.to_owned());
    }
    Ok(result)
}

fn normalize_organization_permissions(
    value: Option<&serde_json::Value>,
) -> Result<Option<OrganizationPermissionsPolicy>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = organization_object(value, "organizationPolicy.permissions must be an object.")?;
    assert_organization_keys(object, &["deny", "ask", "defaultMode", "canPrompt"])?;
    if value
        .get("defaultMode")
        .is_some_and(|value| value != "plan")
    {
        return Err(unsupported_organization(
            "organizationPolicy.permissions.defaultMode may only force plan mode.",
        ));
    }
    if value.get("canPrompt").is_some_and(|value| value != false) {
        return Err(unsupported_organization(
            "organizationPolicy.permissions.canPrompt may only disable prompts.",
        ));
    }
    let permissions = OrganizationPermissionsPolicy {
        deny: organization_string_array(value.get("deny"), "organizationPolicy.permissions.deny")?,
        ask: organization_string_array(value.get("ask"), "organizationPolicy.permissions.ask")?,
        default_plan: value.get("defaultMode").is_some(),
        can_prompt: value.get("canPrompt").map(|_| false),
    };
    Ok((!permissions.deny.is_empty()
        || !permissions.ask.is_empty()
        || permissions.default_plan
        || permissions.can_prompt.is_some())
    .then_some(permissions))
}

fn normalize_organization_selector(
    value: Option<&serde_json::Value>,
    kind: SelectorKind,
) -> Result<Option<SelectorPolicy>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let name = match kind {
        SelectorKind::Models => "models",
        SelectorKind::Tools => "tools",
    };
    let object = organization_object(
        value,
        if kind == SelectorKind::Models {
            "organizationPolicy.models must be an object."
        } else {
            "organizationPolicy.tools must be an object."
        },
    )?;
    assert_organization_keys(object, &["allow", "deny"])?;
    let normalize = |label: &str| -> Result<Vec<String>, SdkSessionError> {
        let values = organization_string_array(
            value.get(label),
            &format!("organizationPolicy.{name}.{label}"),
        )?;
        let regex = match kind {
            SelectorKind::Models => model_selector(),
            SelectorKind::Tools => tool_selector(),
        };
        if values
            .iter()
            .any(|value| value != "*" && !regex.is_match(value))
        {
            return Err(invalid_organization(format!(
                "organizationPolicy.{name}.{label} contains an invalid selector."
            )));
        }
        Ok(values)
    };
    let policy = SelectorPolicy {
        allow: normalize("allow")?,
        deny: normalize("deny")?,
    };
    Ok((!policy.allow.is_empty() || !policy.deny.is_empty()).then_some(policy))
}

fn normalize_organization_setting_sources(
    value: Option<&serde_json::Value>,
) -> Result<Option<SelectorPolicy>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = organization_object(
        value,
        "organizationPolicy.settingSources must be an object.",
    )?;
    assert_organization_keys(object, &["allow", "deny"])?;
    let normalize = |label: &str| -> Result<Vec<String>, SdkSessionError> {
        let values = organization_string_array(
            value.get(label),
            &format!("organizationPolicy.settingSources.{label}"),
        )?;
        if values
            .iter()
            .any(|value| !matches!(value.as_str(), "managed" | "user" | "project" | "local"))
        {
            return Err(invalid_organization(format!(
                "organizationPolicy.settingSources.{label} must contain only managed, user, project, or local."
            )));
        }
        Ok(values)
    };
    let policy = SelectorPolicy {
        allow: normalize("allow")?,
        deny: normalize("deny")?,
    };
    Ok((!policy.allow.is_empty() || !policy.deny.is_empty()).then_some(policy))
}

fn provider_id(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && value
            .chars()
            .all(|char| char.is_ascii_alphanumeric() || "_.:-".contains(char))
}

fn normalize_organization_providers(
    value: Option<&serde_json::Value>,
) -> Result<Option<ProviderPolicy>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = organization_object(value, "organizationPolicy.providers must be an object.")?;
    assert_organization_keys(object, &["allow", "deny", "origins", "credentials"])?;
    let allow =
        organization_string_array(value.get("allow"), "organizationPolicy.providers.allow")?;
    let deny = organization_string_array(value.get("deny"), "organizationPolicy.providers.deny")?;
    if allow.iter().chain(&deny).any(|value| !provider_id(value)) {
        return Err(invalid_organization(
            "organizationPolicy.providers allow/deny must contain exact provider IDs.",
        ));
    }
    let normalize_nested = |key: &str| -> Result<(Vec<String>, Vec<String>), SdkSessionError> {
        let Some(nested) = value.get(key) else {
            return Ok((Vec::new(), Vec::new()));
        };
        let nested = organization_object(
            nested,
            if key == "origins" {
                "organizationPolicy.providers.origins must be an object."
            } else {
                "organizationPolicy.providers.credentials must be an object."
            },
        )?;
        assert_organization_keys(nested, &["allow", "deny"])?;
        Ok((
            organization_string_array(
                nested.get("allow"),
                &format!("organizationPolicy.providers.{key}.allow"),
            )?,
            organization_string_array(
                nested.get("deny"),
                &format!("organizationPolicy.providers.{key}.deny"),
            )?,
        ))
    };
    let (origin_allow, origin_deny) = normalize_nested("origins")?;
    let normalize_origins =
        |values: Vec<String>, label: &str| -> Result<Vec<String>, SdkSessionError> {
            values
                .into_iter()
                .map(|value| {
                    let parsed = url::Url::parse(&value).map_err(|_| {
                        invalid_organization(format!("{label} contains an invalid origin."))
                    })?;
                    if !matches!(parsed.scheme(), "http" | "https")
                        || !parsed.username().is_empty()
                        || parsed.password().is_some()
                        || !matches!(parsed.path(), "" | "/")
                        || parsed.query().is_some()
                        || parsed.fragment().is_some()
                    {
                        return Err(invalid_organization(format!(
                            "{label} must contain credential-free HTTP(S) origins."
                        )));
                    }
                    Ok(parsed.origin().ascii_serialization())
                })
                .collect()
        };
    let origins = SelectorPolicy {
        allow: normalize_origins(origin_allow, "organizationPolicy.providers.origins.allow")?,
        deny: normalize_origins(origin_deny, "organizationPolicy.providers.origins.deny")?,
    };
    let (credential_allow, credential_deny) = normalize_nested("credentials")?;
    for (label, values) in [("allow", &credential_allow), ("deny", &credential_deny)] {
        if values.iter().any(|value| {
            !matches!(
                value.as_str(),
                "environment" | "literal" | "provider_default"
            )
        }) {
            return Err(invalid_organization(format!(
                "organizationPolicy.providers.credentials.{label} contains an invalid credential source."
            )));
        }
    }
    let credentials = SelectorPolicy {
        allow: credential_allow,
        deny: credential_deny,
    };
    let policy = ProviderPolicy {
        allow,
        deny,
        origins,
        credentials,
    };
    Ok((!policy.allow.is_empty()
        || !policy.deny.is_empty()
        || !policy.origins.allow.is_empty()
        || !policy.origins.deny.is_empty()
        || !policy.credentials.allow.is_empty()
        || !policy.credentials.deny.is_empty())
    .then_some(policy))
}

fn json_to_js(value: &serde_json::Value) -> Result<JsValue, SdkSessionError> {
    parse_json(&value.to_string())
        .map_err(|_| invalid_organization("organizationPolicy settings must be JSON values."))
}

fn normalize_organization_session_settings(
    value: Option<&serde_json::Value>,
) -> Result<Option<JsValue>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = json_to_js(value)?;
    super::validate_session_settings(Some(&value))?;
    Ok(Some(value))
}

fn normalize_organization_session_sources(
    value: Option<&serde_json::Value>,
) -> Result<Option<JsValue>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = json_to_js(value)?;
    super::validate_setting_sources(Some(&value))?;
    Ok(Some(value))
}

fn organization_object<'a>(
    value: &'a serde_json::Value,
    message: &'static str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, SdkSessionError> {
    value
        .as_object()
        .ok_or_else(|| SdkSessionError::new("INVALID_GATEWAY_ORGANIZATION_POLICY", message))
}

fn assert_organization_keys(
    object: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
) -> Result<(), SdkSessionError> {
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(SdkSessionError::new(
            "UNSUPPORTED_GATEWAY_ORGANIZATION_POLICY",
            "organizationPolicy contains an unsupported field.",
        ));
    }
    Ok(())
}

fn optional_safe_integer(
    value: Option<&serde_json::Value>,
    allow_zero: bool,
    message: &'static str,
) -> Result<Option<f64>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(number) = value.as_f64() else {
        return Err(SdkSessionError::new(
            "INVALID_GATEWAY_ORGANIZATION_POLICY",
            message,
        ));
    };
    let minimum = if allow_zero { 0.0 } else { 1.0 };
    if !number.is_finite()
        || number < minimum
        || number > 9_007_199_254_740_991.0
        || number.fract() != 0.0
    {
        return Err(SdkSessionError::new(
            "INVALID_GATEWAY_ORGANIZATION_POLICY",
            message,
        ));
    }
    Ok(Some(number))
}

fn optional_positive_finite(
    value: Option<&serde_json::Value>,
    message: &'static str,
) -> Result<Option<f64>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(number) = value.as_f64() else {
        return Err(SdkSessionError::new(
            "INVALID_GATEWAY_ORGANIZATION_POLICY",
            message,
        ));
    };
    if !number.is_finite() || number <= 0.0 {
        return Err(SdkSessionError::new(
            "INVALID_GATEWAY_ORGANIZATION_POLICY",
            message,
        ));
    }
    Ok(Some(number))
}

#[cfg(test)]
mod execution_limit_tests {
    use super::*;

    #[test]
    fn parses_execution_limits() {
        let value = serde_json::json!({
            "limits": {
                "maxTurns": 8,
                "maxSubagentDepth": 0,
                "maxBudgetUsd": 2.5,
                "maxTaskBudgetUsd": 1.25
            },
            "settings": {
                "maxSubagentTimeoutMs": 1250,
                "maxContextTokens": 8192,
                "maxOutputTokens": 2048,
                "maxThinkingTokens": 0,
                "canUpdateLocalSettings": false
            }
        });
        let mut policy = OrganizationPolicy::default();
        policy.apply_execution_limits_json(&value).unwrap();
        assert_eq!(policy.max_turns, Some(8.0));
        assert_eq!(policy.max_subagent_depth, Some(0.0));
        assert_eq!(policy.max_subagent_timeout_ms, Some(1250.0));
        assert_eq!(policy.max_task_budget_usd, Some(1.25));
    }

    #[test]
    fn rejects_unknown_keys() {
        for value in [
            serde_json::json!({"future": true}),
            serde_json::json!({"limits": {"future": true}}),
            serde_json::json!({"settings": {"future": true}}),
        ] {
            let error = OrganizationPolicy::default()
                .apply_execution_limits_json(&value)
                .unwrap_err();
            assert_eq!(error.code, "UNSUPPORTED_GATEWAY_ORGANIZATION_POLICY");
            assert_eq!(
                error.message,
                "organizationPolicy contains an unsupported field."
            );
        }
    }

    #[test]
    fn rejects_each_invalid_execution_limit_with_the_gateway_code() {
        for (value, message) in [
            (
                serde_json::json!({"limits": {"maxTurns": 0}}),
                "organizationPolicy.limits.maxTurns must be a positive safe integer.",
            ),
            (
                serde_json::json!({"limits": {"maxSubagentDepth": -1}}),
                "organizationPolicy.limits.maxSubagentDepth must be a non-negative safe integer.",
            ),
            (
                serde_json::json!({"settings": {"maxSubagentTimeoutMs": 1.5}}),
                "organizationPolicy.settings.maxSubagentTimeoutMs must be a positive safe integer.",
            ),
            (
                serde_json::json!({"limits": {"maxBudgetUsd": 0}}),
                "organizationPolicy.limits.maxBudgetUsd must be a positive finite number.",
            ),
            (
                serde_json::json!({"limits": {"maxTaskBudgetUsd": "1"}}),
                "organizationPolicy.limits.maxTaskBudgetUsd must be a positive finite number.",
            ),
            (
                serde_json::json!({"settings": {"maxContextTokens": 0}}),
                "organizationPolicy.settings.maxContextTokens must be a positive safe integer.",
            ),
            (
                serde_json::json!({"settings": {"maxOutputTokens": 1.5}}),
                "organizationPolicy.settings.maxOutputTokens must be a positive safe integer.",
            ),
            (
                serde_json::json!({"settings": {"maxThinkingTokens": -1}}),
                "organizationPolicy.settings.maxThinkingTokens must be a non-negative safe integer.",
            ),
        ] {
            let error = OrganizationPolicy::default()
                .apply_execution_limits_json(&value)
                .unwrap_err();
            assert_eq!(error.code, "INVALID_GATEWAY_ORGANIZATION_POLICY");
            assert_eq!(error.message, message);
        }
    }

    #[test]
    fn can_update_local_settings_may_only_disable_updates() {
        let error = OrganizationPolicy::default()
            .apply_execution_limits_json(
                &serde_json::json!({"settings": {"canUpdateLocalSettings": true}}),
            )
            .unwrap_err();
        assert_eq!(error.code, "UNSUPPORTED_GATEWAY_ORGANIZATION_POLICY");
        assert_eq!(
            error.message,
            "organizationPolicy.settings.canUpdateLocalSettings may only disable SDK settings updates."
        );
    }

    #[test]
    fn normalizes_every_organization_policy_domain_and_runtime_cap() {
        let value = serde_json::json!({
            "permissions": {
                "deny": ["Bash(rm *)"],
                "ask": ["Write"],
                "defaultMode": "plan",
                "canPrompt": false
            },
            "models": {"allow": ["p/*"], "deny": ["p/blocked"]},
            "providers": {
                "allow": ["p"],
                "deny": ["disabled"],
                "origins": {"allow": ["https://api.example.test/"]},
                "credentials": {"allow": ["environment"], "deny": ["literal"]}
            },
            "tools": {"allow": ["read_*", "agent"], "deny": ["read_secret"]},
            "settingSources": {"allow": ["managed", "project"], "deny": ["local"]},
            "limits": {
                "maxTurns": 8,
                "maxBudgetUsd": 2.5,
                "maxTaskBudgetUsd": 1.25,
                "maxSubagentDepth": 2
            },
            "settings": {
                "canUpdateLocalSettings": false,
                "maxContextTokens": 8192,
                "maxOutputTokens": 2048,
                "maxThinkingTokens": 0,
                "maxSubagentTimeoutMs": 1250,
                "sessionDefaults": {"agent": {"maxContextTokens": 4096}},
                "managedSessionSettings": {"agent": {"maxOutputTokens": 1024}},
                "sessionDefaultSources": ["managed", "project"],
                "enforcedSessionSettings": {"agent": {"subagents": {"maxDepth": 1}}}
            }
        });
        let mut policy = OrganizationPolicy::default();
        policy.apply_execution_limits_json(&value).unwrap();
        let permissions = policy.permissions.as_ref().unwrap();
        assert_eq!(permissions.deny, ["Bash(rm *)"]);
        assert_eq!(permissions.ask, ["Write"]);
        assert!(permissions.default_plan);
        assert_eq!(permissions.can_prompt, Some(false));
        assert_eq!(policy.models.as_ref().unwrap().allow, ["p/*"]);
        assert_eq!(policy.tools.as_ref().unwrap().allow, ["read_*", "agent"]);
        assert_eq!(
            policy.providers.as_ref().unwrap().origins.allow,
            ["https://api.example.test"]
        );
        assert_eq!(policy.max_budget_usd, Some(2.5));
        let settings = policy.settings.as_ref().unwrap();
        assert_eq!(settings.max_context_tokens, Some(8192.0));
        assert_eq!(settings.max_output_tokens, Some(2048.0));
        assert_eq!(settings.max_thinking_tokens, Some(0.0));
        assert!(settings.session_defaults.is_some());
        assert!(settings.managed_session_settings.is_some());
        assert!(settings.session_default_sources.is_some());
        assert!(settings.enforced_session_settings.is_some());

        let mut agent = parse_json(
            r#"{"maxContextTokens":16384,"maxOutputTokens":1024,"thinking":{"enabled":true,"mode":"high","budgetTokens":512}}"#,
        )
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
        policy.apply_agent_runtime_caps(&mut agent);
        assert_eq!(
            agent.get("maxContextTokens"),
            Some(&JsValue::Number(8192.0))
        );
        assert_eq!(agent.get("maxOutputTokens"), Some(&JsValue::Number(1024.0)));
        assert_eq!(
            agent.get("thinking").unwrap().get("enabled"),
            Some(&JsValue::Bool(false))
        );
        assert_eq!(
            agent.get("thinking").unwrap().get("mode"),
            Some(&"off".into())
        );
        assert!(agent.get("thinking").unwrap().get("budgetTokens").is_none());

        let rules = policy.permission_rules();
        assert_eq!(rules.deny[0].tool_name, "bash".into());
        assert_eq!(rules.deny[0].pattern, Some("rm *".into()));
        assert_eq!(rules.ask[0].tool_name, "write_file".into());
    }

    #[test]
    fn rejects_invalid_nested_organization_policy_values_fail_closed() {
        for (value, code, message) in [
            (
                serde_json::json!({"permissions": {"defaultMode": "default"}}),
                "UNSUPPORTED_GATEWAY_ORGANIZATION_POLICY",
                "organizationPolicy.permissions.defaultMode may only force plan mode.",
            ),
            (
                serde_json::json!({"permissions": {"deny": ["bash", " bash "]}}),
                "INVALID_GATEWAY_ORGANIZATION_POLICY",
                "organizationPolicy.permissions.deny cannot contain duplicate entries.",
            ),
            (
                serde_json::json!({"models": {"allow": ["not-a-model"]}}),
                "INVALID_GATEWAY_ORGANIZATION_POLICY",
                "organizationPolicy.models.allow contains an invalid selector.",
            ),
            (
                serde_json::json!({"tools": {"deny": ["bad selector"]}}),
                "INVALID_GATEWAY_ORGANIZATION_POLICY",
                "organizationPolicy.tools.deny contains an invalid selector.",
            ),
            (
                serde_json::json!({"providers": {"origins": {"allow": ["https://user@example.test"]}}}),
                "INVALID_GATEWAY_ORGANIZATION_POLICY",
                "organizationPolicy.providers.origins.allow must contain credential-free HTTP(S) origins.",
            ),
            (
                serde_json::json!({"providers": {"credentials": {"deny": ["file"]}}}),
                "INVALID_GATEWAY_ORGANIZATION_POLICY",
                "organizationPolicy.providers.credentials.deny contains an invalid credential source.",
            ),
            (
                serde_json::json!({"settingSources": {"allow": ["remote"]}}),
                "INVALID_GATEWAY_ORGANIZATION_POLICY",
                "organizationPolicy.settingSources.allow must contain only managed, user, project, or local.",
            ),
        ] {
            let error = OrganizationPolicy::default()
                .apply_execution_limits_json(&value)
                .unwrap_err();
            assert_eq!(error.code, code);
            assert_eq!(error.message, message);
        }
    }
}
