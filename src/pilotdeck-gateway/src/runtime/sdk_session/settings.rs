use super::{
    SdkSessionError, array, field, is_integer, is_null, is_record, is_safe_integer, js_object,
    nullish, number_text, own, record, settings_error, trim_units, truthy,
};
use crate::{config::Env, metadata::resolve};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

// ─────────────────────────────────────────────────────────── setting sources

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SettingSource {
    Managed,
    User,
    Project,
    Local,
}

impl SettingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::User => "user",
            Self::Project => "project",
            Self::Local => "local",
        }
    }
}

const SOURCE_PRECEDENCE: [SettingSource; 4] = [
    SettingSource::Managed,
    SettingSource::User,
    SettingSource::Project,
    SettingSource::Local,
];

fn invalid_sources() -> SdkSessionError {
    settings_error(
        "INVALID_SDK_SETTING_SOURCES",
        "settingSources must be a non-empty array containing managed, user, project, and/or local.",
    )
}

fn unsupported_source() -> SdkSessionError {
    settings_error(
        "UNSUPPORTED_SDK_SETTING_SOURCE",
        "PilotDeck settingSources supports only Gateway-owned managed, user, project, and local sources.",
    )
}

fn duplicate_sources() -> SdkSessionError {
    settings_error(
        "INVALID_SDK_SETTING_SOURCES",
        "settingSources cannot contain duplicate entries.",
    )
}

fn parse_source(value: &JsValue) -> Result<SettingSource, SdkSessionError> {
    let Some(text) = value.as_string() else {
        return Err(unsupported_source());
    };
    if text.equals_str("managed") {
        Ok(SettingSource::Managed)
    } else if text.equals_str("user") {
        Ok(SettingSource::User)
    } else if text.equals_str("project") {
        Ok(SettingSource::Project)
    } else if text.equals_str("local") {
        Ok(SettingSource::Local)
    } else {
        Err(unsupported_source())
    }
}

fn normalize_sources(value: Option<&JsValue>) -> Result<Vec<SettingSource>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_undefined() {
        return Ok(Vec::new());
    }
    let Some(items) = array(value) else {
        return Err(invalid_sources());
    };
    if items.is_empty() {
        return Err(invalid_sources());
    }
    let mut selected = Vec::with_capacity(items.len());
    for item in items {
        selected.push(parse_source(item)?);
    }
    let unique: HashSet<SettingSource> = selected.iter().copied().collect();
    if unique.len() != selected.len() {
        return Err(duplicate_sources());
    }
    Ok(SOURCE_PRECEDENCE
        .into_iter()
        .filter(|source| selected.contains(source))
        .collect())
}

/// `validatePilotSdkSettingSources`: validates without resolving host paths.
pub fn validate_setting_sources(value: Option<&JsValue>) -> Result<(), SdkSessionError> {
    normalize_sources(value).map(|_| ())
}

fn merge_sources(
    host: Option<&JsValue>,
    sdk: Option<&JsValue>,
) -> Result<Vec<SettingSource>, SdkSessionError> {
    let mut selected = normalize_sources(host)?;
    for source in normalize_sources(sdk)? {
        if !selected.contains(&source) {
            selected.push(source);
        }
    }
    Ok(SOURCE_PRECEDENCE
        .into_iter()
        .filter(|source| selected.contains(source))
        .collect())
}

// ─────────────────────────────────────────────────────────── settings validation

fn invalid_settings(label: &str, detail: &str) -> SdkSessionError {
    settings_error("INVALID_SDK_SETTINGS", format!("{label}.{detail}"))
}

/// `normalizeSettings(value, label)`. Returns `None` for `undefined`, `{}` for
/// an accepted empty overlay, and the normalized `{agent: ...}` otherwise.
pub fn normalize_settings(
    value: Option<&JsValue>,
    label: &str,
) -> Result<Option<JsValue>, SdkSessionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_undefined() {
        return Ok(None);
    }
    let Some(object) = record(value) else {
        return Err(settings_error(
            "INVALID_SDK_SETTINGS",
            format!("{label} must be an object."),
        ));
    };
    for (key, _) in object.own_entries() {
        if !key.equals_str("agent") {
            return Err(settings_error(
                "UNSUPPORTED_SDK_SETTING",
                format!(
                    "{label}.{} is not supported; only non-secret agent model, limits, thinking, and subagent settings may be overlaid.",
                    number_text(key)
                ),
            ));
        }
    }
    let Some(agent_value) = own(value, "agent") else {
        return Ok(Some(JsValue::Object(JsObject::new())));
    };
    if agent_value.is_undefined() {
        return Ok(Some(JsValue::Object(JsObject::new())));
    }
    let Some(agent) = record(agent_value) else {
        return Err(settings_error(
            "INVALID_SDK_SETTINGS",
            format!("{label}.agent must be an object."),
        ));
    };
    for (key, _) in agent.own_entries() {
        if !matches!(
            key.to_utf8().ok().as_deref(),
            Some("model")
                | Some("fallbackModel")
                | Some("maxContextTokens")
                | Some("maxOutputTokens")
                | Some("thinking")
                | Some("subagents")
        ) {
            return Err(settings_error(
                "UNSUPPORTED_SDK_SETTING",
                format!(
                    "{label}.agent.{} is not supported by the SDK session overlay.",
                    number_text(key)
                ),
            ));
        }
    }

    let mut output = JsObject::new();
    for (key, detail) in [
        ("model", "agent.model"),
        ("fallbackModel", "agent.fallbackModel"),
    ] {
        let Some(setting) = own(agent_value, key).filter(|value| !value.is_undefined()) else {
            continue;
        };
        if is_null(setting) {
            output.insert(key.into(), JsValue::Null);
            continue;
        }
        let Some(text) = setting.as_string() else {
            return Err(invalid_settings(
                label,
                &format!("{detail} must be a non-empty model id or null."),
            ));
        };
        let trimmed = trim_units(text);
        if trimmed.is_empty() {
            return Err(invalid_settings(
                label,
                &format!("{detail} must be a non-empty model id or null."),
            ));
        }
        output.insert(key.into(), JsValue::String(trimmed));
    }

    for key in ["maxContextTokens", "maxOutputTokens"] {
        let Some(setting) = own(agent_value, key).filter(|value| !value.is_undefined()) else {
            continue;
        };
        let valid = setting
            .as_number()
            .is_some_and(|value| is_integer(value) && value > 0.0);
        if !valid {
            return Err(invalid_settings(
                label,
                &format!("agent.{key} must be a positive integer."),
            ));
        }
        output.insert(key.into(), setting.clone());
    }

    if let Some(thinking) = own(agent_value, "thinking").filter(|value| !value.is_undefined()) {
        let Some(thinking_object) = record(thinking) else {
            return Err(invalid_settings(
                label,
                "agent.thinking must include a boolean enabled field.",
            ));
        };
        if thinking_object
            .get("enabled")
            .and_then(JsValue::as_bool)
            .is_none()
        {
            return Err(invalid_settings(
                label,
                "agent.thinking must include a boolean enabled field.",
            ));
        }
        for (key, _) in thinking_object.own_entries() {
            if !key.equals_str("enabled") && !key.equals_str("budgetTokens") {
                return Err(settings_error(
                    "UNSUPPORTED_SDK_SETTING",
                    format!(
                        "{label}.agent.thinking.{} is not supported by the SDK session overlay.",
                        number_text(key)
                    ),
                ));
            }
        }
        let mut normalized = JsObject::new();
        normalized.insert(
            "enabled".into(),
            thinking_object
                .get("enabled")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        );
        if let Some(budget) = thinking_object
            .get("budgetTokens")
            .filter(|value| !value.is_undefined())
        {
            let valid = budget
                .as_number()
                .is_some_and(|value| is_integer(value) && value >= 0.0);
            if !valid {
                return Err(invalid_settings(
                    label,
                    "agent.thinking.budgetTokens must be a non-negative integer.",
                ));
            }
            normalized.insert("budgetTokens".into(), budget.clone());
        }
        output.insert("thinking".into(), JsValue::Object(normalized));
    }

    if let Some(subagents) = own(agent_value, "subagents").filter(|value| !value.is_undefined()) {
        let Some(subagents_object) = record(subagents) else {
            return Err(invalid_settings(
                label,
                "agent.subagents must be an object.",
            ));
        };
        for (key, _) in subagents_object.own_entries() {
            if !key.equals_str("default")
                && !key.equals_str("timeoutMs")
                && !key.equals_str("maxDepth")
            {
                return Err(settings_error(
                    "UNSUPPORTED_SDK_SETTING",
                    format!(
                        "{label}.agent.subagents supports only default, timeoutMs, and maxDepth for the SDK session overlay."
                    ),
                ));
            }
        }
        let default_value = subagents_object
            .get("default")
            .filter(|value| !value.is_undefined());
        let timeout = subagents_object
            .get("timeoutMs")
            .filter(|value| !value.is_undefined());
        let depth = subagents_object
            .get("maxDepth")
            .filter(|value| !value.is_undefined());
        let mut normalized = JsObject::new();
        if let Some(default_value) = default_value {
            let value = if is_null(default_value) {
                JsValue::Null
            } else {
                let Some(text) = default_value.as_string() else {
                    return Err(invalid_settings(
                        label,
                        "agent.subagents.default must be a non-empty model id, \"inherit\", or null.",
                    ));
                };
                let trimmed = trim_units(text);
                if trimmed.is_empty() {
                    return Err(invalid_settings(
                        label,
                        "agent.subagents.default must be a non-empty model id, \"inherit\", or null.",
                    ));
                }
                if trimmed.equals_str("inherit") {
                    JsValue::Null
                } else {
                    JsValue::String(trimmed)
                }
            };
            normalized.insert("default".into(), value);
        }
        if let Some(timeout) = timeout {
            let valid = timeout
                .as_number()
                .is_some_and(|value| is_integer(value) && value > 0.0);
            if !valid {
                return Err(invalid_settings(
                    label,
                    "agent.subagents.timeoutMs must be a positive integer.",
                ));
            }
            normalized.insert("timeoutMs".into(), timeout.clone());
        }
        if let Some(depth) = depth {
            let valid = depth
                .as_number()
                .is_some_and(|value| is_safe_integer(value) && value >= 0.0);
            if !valid {
                return Err(invalid_settings(
                    label,
                    "agent.subagents.maxDepth must be a non-negative safe integer.",
                ));
            }
            normalized.insert("maxDepth".into(), depth.clone());
        }
        if !normalized.is_empty() {
            output.insert("subagents".into(), JsValue::Object(normalized));
        }
    }

    if output.is_empty() {
        Ok(Some(JsValue::Object(JsObject::new())))
    } else {
        Ok(Some(js_object(vec![("agent", JsValue::Object(output))])))
    }
}

/// `validatePilotSdkSessionSettings`.
pub fn validate_session_settings(value: Option<&JsValue>) -> Result<(), SdkSessionError> {
    normalize_settings(value, "settings").map(|_| ())
}

fn merge_settings(base: Option<JsValue>, override_value: Option<JsValue>) -> Option<JsValue> {
    match (base, override_value) {
        (None, None) => None,
        (None, Some(value)) => Some(value),
        (Some(value), None) => Some(value),
        (Some(base), Some(override_value)) => {
            let mut agent = JsObject::new();
            if let Some(base_agent) = field(&base, "agent").as_object() {
                for (key, value) in base_agent.entries() {
                    agent.insert(key.clone(), value.clone());
                }
            }
            if let Some(override_agent) = field(&override_value, "agent").as_object() {
                for (key, value) in override_agent.entries() {
                    agent.insert(key.clone(), value.clone());
                }
            }
            let base_subagents = field(&base, "agent")
                .as_object()
                .and_then(|agent| agent.get("subagents"))
                .filter(|value| truthy(value));
            let override_subagents = field(&override_value, "agent")
                .as_object()
                .and_then(|agent| agent.get("subagents"))
                .filter(|value| truthy(value));
            if base_subagents.is_some() || override_subagents.is_some() {
                let mut subagents = JsObject::new();
                if let Some(base) = base_subagents.and_then(JsValue::as_object) {
                    for (key, value) in base.entries() {
                        subagents.insert(key.clone(), value.clone());
                    }
                }
                if let Some(override_value) = override_subagents.and_then(JsValue::as_object) {
                    for (key, value) in override_value.entries() {
                        subagents.insert(key.clone(), value.clone());
                    }
                }
                agent.insert("subagents".into(), JsValue::Object(subagents));
            }
            Some(js_object(vec![("agent", JsValue::Object(agent))]))
        }
    }
}

fn has_settings(value: &Option<JsValue>) -> bool {
    value
        .as_ref()
        .and_then(|value| value.get("agent"))
        .and_then(JsValue::as_object)
        .is_some_and(|agent| !agent.is_empty())
}

/// `extractSourceOverlay` — copies only the SDK overlay namespace.
fn extract_source_overlay(value: &JsValue) -> JsValue {
    let Some(agent_value) = own(value, "agent") else {
        return JsValue::Object(JsObject::new());
    };
    if agent_value.is_undefined() {
        return JsValue::Object(JsObject::new());
    }
    let Some(agent) = record(agent_value) else {
        return js_object(vec![("agent", agent_value.clone())]);
    };
    let mut overlay = JsObject::new();
    for key in [
        "model",
        "fallbackModel",
        "maxContextTokens",
        "maxOutputTokens",
    ] {
        if let Some(value) = agent.get(key).filter(|value| !value.is_undefined()) {
            overlay.insert(key.into(), value.clone());
        }
    }
    if let Some(thinking) = agent.get("thinking").filter(|value| !value.is_undefined()) {
        if !is_record(thinking) {
            overlay.insert("thinking".into(), thinking.clone());
        } else {
            let mut normalized = JsObject::new();
            for key in ["enabled", "budgetTokens"] {
                if let Some(value) = thinking.get(key).filter(|value| !value.is_undefined()) {
                    normalized.insert(key.into(), value.clone());
                }
            }
            overlay.insert("thinking".into(), JsValue::Object(normalized));
        }
    }
    if let Some(subagents) = agent.get("subagents").filter(|value| !value.is_undefined()) {
        if !is_record(subagents) {
            overlay.insert("subagents".into(), subagents.clone());
        } else {
            let mut normalized = JsObject::new();
            for key in ["default", "timeoutMs", "maxDepth"] {
                if let Some(value) = subagents.get(key).filter(|value| !value.is_undefined()) {
                    normalized.insert(key.into(), value.clone());
                }
            }
            if !normalized.is_empty() {
                overlay.insert("subagents".into(), JsValue::Object(normalized));
            }
        }
    }
    js_object(vec![("agent", JsValue::Object(overlay))])
}

// ─────────────────────────────────────────────────────────── settings resolution

fn resolve_pilot_home(env: &Env, cwd: &Path) -> PathBuf {
    let raw = env
        .get("PILOT_HOME")
        .map(String::as_str)
        .unwrap_or("~/.pilotdeck");
    if raw == "~" {
        return home_dir(env);
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return resolve(&home_dir(env), rest);
    }
    resolve(cwd, raw)
}

pub(super) fn home_dir(env: &Env) -> PathBuf {
    env.get("HOME")
        .map(PathBuf::from)
        // `os.homedir()` reads the process HOME when the injected env omits it.
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn source_path(source: SettingSource, project_root: &Path, env: &Env, cwd: &Path) -> PathBuf {
    match source {
        SettingSource::User => resolve(&resolve_pilot_home(env, cwd), "pilotdeck.yaml"),
        SettingSource::Project => resolve(project_root, ".pilotdeck/pilotdeck.yaml"),
        SettingSource::Local => resolve(project_root, ".pilotdeck/pilotdeck.local.yaml"),
        SettingSource::Managed => unreachable!("managed settings are host-owned"),
    }
}

/// Node's `readFileSync` diagnostic, exactly as `node_io` recorded it. Unknown
/// errnos stay a host boundary and keep Rust's Display text.
pub(super) fn read_failure_message(error: &std::io::Error, path: &Path) -> String {
    crate::node_io::read_file(error, &path.to_string_lossy())
        .map(|(_, message)| message)
        .unwrap_or_else(|| error.to_string())
}

fn read_source_settings(
    source: SettingSource,
    project_root: &Path,
    env: &Env,
    cwd: &Path,
) -> Result<Option<JsValue>, SdkSessionError> {
    let path = source_path(source, project_root, env, cwd);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(|error| {
        settings_error(
            "SDK_SETTING_SOURCE_READ_FAILED",
            format!(
                "Unable to read {} settings source: {}",
                source.as_str(),
                read_failure_message(&error, &path)
            ),
        )
    })?;
    let content = JsString::from(String::from_utf8_lossy(&bytes).into_owned());
    let document = pilotdeck_yaml::parse_document(
        &content,
        pilotdeck_yaml::Options {
            pretty_errors: false,
            ..Default::default()
        },
    );
    if !document.errors.is_empty() {
        let messages = document
            .errors
            .iter()
            .map(|error| String::from_utf16_lossy(error.message.units()))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(settings_error(
            "SDK_SETTING_SOURCE_YAML_INVALID",
            format!(
                "Unable to parse {} settings source: {messages}",
                source.as_str()
            ),
        ));
    }
    // `document.toJSON()` is outside the baseline's try/catch: conversion
    // failures keep their original JavaScript name and carry no gateway code.
    let parsed = document.to_json().map_err(|error| {
        SdkSessionError::raw(error.name, String::from_utf16_lossy(error.message.units()))
    })?;
    let parsed = parsed.to_js_value().map_err(|error| {
        SdkSessionError::raw(error.name, String::from_utf16_lossy(error.message.units()))
    })?;
    if nullish(&parsed) {
        return Ok(None);
    }
    if !is_record(&parsed) {
        return Err(settings_error(
            "SDK_SETTING_SOURCE_INVALID",
            format!(
                "{} settings source must have an object root.",
                source.as_str()
            ),
        ));
    }
    let overlay = extract_source_overlay(&parsed);
    normalize_settings(
        Some(&overlay),
        &format!("{} settings source", source.as_str()),
    )
}

/// Inputs for `resolvePilotSdkSessionSettings`. `None` means the policy/value
/// was absent; `Some(JsValue::Undefined)` is treated exactly like JS does.
pub struct ResolveSettingsOptions<'a> {
    pub project_root: &'a Path,
    pub cwd: &'a Path,
    pub env: &'a Env,
    pub host_defaults: Option<&'a JsValue>,
    pub host_managed_settings: Option<&'a JsValue>,
    pub host_setting_sources: Option<&'a JsValue>,
    pub settings: Option<&'a JsValue>,
    pub setting_sources: Option<&'a JsValue>,
    pub host_enforced_settings: Option<&'a JsValue>,
}

/// Port of `resolvePilotSdkSessionSettings`.
pub fn resolve_session_settings(
    options: ResolveSettingsOptions<'_>,
) -> Result<Option<JsValue>, SdkSessionError> {
    let sources = merge_sources(options.host_setting_sources, options.setting_sources)?;
    let mut resolved = merge_settings(
        None,
        normalize_settings(options.host_defaults, "hostDefaults")?,
    );
    let managed = normalize_settings(options.host_managed_settings, "hostManagedSettings")?;
    for source in sources {
        let source_settings = match source {
            SettingSource::Managed => managed.clone(),
            other => read_source_settings(other, options.project_root, options.env, options.cwd)?,
        };
        resolved = merge_settings(resolved, source_settings);
    }
    resolved = merge_settings(resolved, normalize_settings(options.settings, "settings")?);
    resolved = merge_settings(
        resolved,
        normalize_settings(options.host_enforced_settings, "hostEnforcedSettings")?,
    );
    Ok(if has_settings(&resolved) {
        resolved
    } else {
        None
    })
}
