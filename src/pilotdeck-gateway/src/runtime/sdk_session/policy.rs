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

#[derive(Clone, Debug, Default)]
pub struct OrganizationPolicy {
    pub models: Option<SelectorPolicy>,
    pub providers: Option<ProviderPolicy>,
    pub setting_sources: Option<SelectorPolicy>,
    pub settings: Option<OrganizationSettingsPolicy>,
    pub max_task_budget_usd: Option<f64>,
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

    /// Parse `organizationPolicy` from gateway JSON (execution limits file).
    pub fn apply_execution_limits_json(
        &mut self,
        value: &serde_json::Value,
    ) -> Result<(), SdkSessionError> {
        let policy = organization_json_object(value, "organizationPolicy must be an object.")?; // file body is the policy object
        self.models = normalize_organization_selector_json(policy.get("models"), SelectorKind::Models)?;
        self.providers = normalize_organization_providers_json(policy.get("providers"))?;
        self.setting_sources =
            normalize_organization_setting_sources_json(policy.get("settingSources"))?;
        let limits = policy
            .get("limits")
            .map(|value| organization_json_object(value, "organizationPolicy.limits must be an object."))
            .transpose()?;
        let settings = policy
            .get("settings")
            .map(|value| {
                organization_json_object(value, "organizationPolicy.settings must be an object.")
            })
            .transpose()?;
        self.max_turns = optional_json_safe_integer(
            limits.as_ref().and_then(|limits| limits.get("maxTurns")),
            false,
            "organizationPolicy.limits.maxTurns must be a positive safe integer.",
        )?;
        self.max_subagent_depth = optional_json_safe_integer(
            limits.as_ref().and_then(|limits| limits.get("maxSubagentDepth")),
            true,
            "organizationPolicy.limits.maxSubagentDepth must be a non-negative safe integer.",
        )?;
        self.max_task_budget_usd = optional_json_positive_finite(
            limits.as_ref().and_then(|limits| limits.get("maxTaskBudgetUsd")),
            "organizationPolicy.limits.maxTaskBudgetUsd must be a positive finite number.",
        )?;
        self.max_subagent_timeout_ms = optional_json_safe_integer(
            settings.as_ref().and_then(|settings| settings.get("maxSubagentTimeoutMs")),
            false,
            "organizationPolicy.settings.maxSubagentTimeoutMs must be a positive safe integer.",
        )?;
        if settings.as_ref().and_then(|s| s.get("canUpdateLocalSettings")).is_some_and(|value| {
            value != &serde_json::Value::Bool(false)
        }) {
            return Err(SdkSessionError::new(
                "UNSUPPORTED_GATEWAY_ORGANIZATION_POLICY",
                "organizationPolicy.settings.canUpdateLocalSettings may only disable SDK settings updates.",
            ));
        }
        let max_context_tokens = optional_json_safe_integer(
            settings.as_ref().and_then(|settings| settings.get("maxContextTokens")),
            false,
            "organizationPolicy.settings.maxContextTokens must be a positive safe integer.",
        )?;
        let max_output_tokens = optional_json_safe_integer(
            settings.as_ref().and_then(|settings| settings.get("maxOutputTokens")),
            false,
            "organizationPolicy.settings.maxOutputTokens must be a positive safe integer.",
        )?;
        let max_thinking_tokens = optional_json_safe_integer(
            settings.as_ref().and_then(|settings| settings.get("maxThinkingTokens")),
            true,
            "organizationPolicy.settings.maxThinkingTokens must be a non-negative safe integer.",
        )?;
        let session_defaults = normalize_organization_session_settings_json(
            settings.as_ref().and_then(|settings| settings.get("sessionDefaults")),
        )?;
        let managed_session_settings = normalize_organization_session_settings_json(
            settings.as_ref().and_then(|settings| settings.get("managedSessionSettings")),
        )?;
        let enforced_session_settings = normalize_organization_session_settings_json(
            settings.as_ref().and_then(|settings| settings.get("enforcedSessionSettings")),
        )?;
        let session_default_sources = normalize_organization_session_sources_json(
            settings.as_ref().and_then(|settings| settings.get("sessionDefaultSources")),
        )?;
        let can_update_local_settings = settings
            .as_ref()
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
        self.settings = organization_settings_has_json_values(&organization_settings)
            .then_some(organization_settings);
        Ok(())
    }
}

fn invalid_organization_json(message: impl Into<String>) -> SdkSessionError {
    SdkSessionError::new("INVALID_GATEWAY_ORGANIZATION_POLICY", message)
}

fn organization_json_object<'a>(
    value: &'a serde_json::Value,
    message: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, SdkSessionError> {
    value
        .as_object()
        .ok_or_else(|| invalid_organization_json(message))
}

fn organization_settings_has_json_values(settings: &OrganizationSettingsPolicy) -> bool {
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

fn organization_json_string_array(
    value: Option<&serde_json::Value>,
    label: &str,
) -> Result<Vec<String>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(invalid_organization_json(format!(
            "{label} must be an array of non-empty strings."
        )));
    };
    let mut result = Vec::with_capacity(items.len());
    for item in items {
        let Some(item) = item.as_str() else {
            return Err(invalid_organization_json(format!(
                "{label} must be an array of non-empty strings."
            )));
        };
        let item = item.trim();
        if item.is_empty() {
            return Err(invalid_organization_json(format!(
                "{label} must be an array of non-empty strings."
            )));
        }
        if result.iter().any(|existing| existing == item) {
            return Err(invalid_organization_json(format!(
                "{label} cannot contain duplicate entries."
            )));
        }
        result.push(item.to_owned());
    }
    Ok(result)
}

fn normalize_organization_selector_json(
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
    organization_json_object(value, &format!("organizationPolicy.{name} must be an object."))?;
    let regex = match kind {
        SelectorKind::Models => model_selector(),
        SelectorKind::Tools => tool_selector(),
    };
    let normalize = |label: &str| -> Result<Vec<String>, SdkSessionError> {
        let values = organization_json_string_array(
            value.get(label),
            &format!("organizationPolicy.{name}.{label}"),
        )?;
        if values
            .iter()
            .any(|value| value != "*" && !regex.is_match(value))
        {
            return Err(invalid_organization_json(format!(
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

fn normalize_organization_setting_sources_json(
    value: Option<&serde_json::Value>,
) -> Result<Option<SelectorPolicy>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    organization_json_object(
        value,
        "organizationPolicy.settingSources must be an object.",
    )?;
    let policy = SelectorPolicy {
        allow: organization_json_string_array(
            value.get("allow"),
            "organizationPolicy.settingSources.allow",
        )?,
        deny: organization_json_string_array(
            value.get("deny"),
            "organizationPolicy.settingSources.deny",
        )?,
    };
    Ok((!policy.allow.is_empty() || !policy.deny.is_empty()).then_some(policy))
}

fn normalize_organization_providers_json(
    value: Option<&serde_json::Value>,
) -> Result<Option<ProviderPolicy>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    organization_json_object(value, "organizationPolicy.providers must be an object.")?;
    let policy = ProviderPolicy {
        allow: organization_json_string_array(
            value.get("allow"),
            "organizationPolicy.providers.allow",
        )?,
        deny: organization_json_string_array(value.get("deny"), "organizationPolicy.providers.deny")?,
        origins: SelectorPolicy {
            allow: organization_json_string_array(
                value.get("origins").and_then(|v| v.get("allow")),
                "organizationPolicy.providers.origins.allow",
            )?,
            deny: organization_json_string_array(
                value.get("origins").and_then(|v| v.get("deny")),
                "organizationPolicy.providers.origins.deny",
            )?,
        },
        credentials: SelectorPolicy {
            allow: organization_json_string_array(
                value.get("credentials").and_then(|v| v.get("allow")),
                "organizationPolicy.providers.credentials.allow",
            )?,
            deny: organization_json_string_array(
                value.get("credentials").and_then(|v| v.get("deny")),
                "organizationPolicy.providers.credentials.deny",
            )?,
        },
    };
    let nonempty = !policy.allow.is_empty()
        || !policy.deny.is_empty()
        || !policy.origins.allow.is_empty()
        || !policy.origins.deny.is_empty()
        || !policy.credentials.allow.is_empty()
        || !policy.credentials.deny.is_empty();
    Ok(nonempty.then_some(policy))
}

fn optional_json_safe_integer(
    value: Option<&serde_json::Value>,
    allow_zero: bool,
    message: &str,
) -> Result<Option<f64>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(number) = value.as_f64() else {
        return Err(invalid_organization_json(message));
    };
    if !number.is_finite() || number.fract() != 0.0 {
        return Err(invalid_organization_json(message));
    }
    if allow_zero {
        if number < 0.0 {
            return Err(invalid_organization_json(message));
        }
    } else if number <= 0.0 {
        return Err(invalid_organization_json(message));
    }
    Ok(Some(number))
}

fn optional_json_positive_finite(
    value: Option<&serde_json::Value>,
    message: &str,
) -> Result<Option<f64>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(number) = value.as_f64() else {
        return Err(invalid_organization_json(message));
    };
    if !number.is_finite() || number <= 0.0 {
        return Err(invalid_organization_json(message));
    }
    Ok(Some(number))
}

fn json_to_js(value: &serde_json::Value) -> Result<JsValue, SdkSessionError> {
    parse_json(&value.to_string()).map_err(|_| {
        invalid_organization_json("organizationPolicy settings must be JSON values.")
    })
}

fn normalize_organization_session_settings_json(
    value: Option<&serde_json::Value>,
) -> Result<Option<JsValue>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = json_to_js(value)?;
    super::validate_session_settings(Some(&value))?;
    Ok(Some(value))
}

fn normalize_organization_session_sources_json(
    value: Option<&serde_json::Value>,
) -> Result<Option<JsValue>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = json_to_js(value)?;
    super::validate_setting_sources(Some(&value))?;
    Ok(Some(value))
}
