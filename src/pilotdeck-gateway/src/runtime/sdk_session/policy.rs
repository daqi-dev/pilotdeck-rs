use super::{
    SdkSessionError, array, field, math_min, number, number_text, own, set_property, slash,
    trim_units, truthy,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
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
}
