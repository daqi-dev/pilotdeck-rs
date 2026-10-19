//! SDK session configuration and validation, ported from the fixed TypeScript
//! baseline:
//!
//! - `src/cli/ProjectRuntimeRegistry.ts:776-1056` (state clearing, handler
//!   registration scaffolding, `assertSdkModelAllowed`,
//!   `setSessionThinkingForSdk`, `applyFlagSettingsForSdk`,
//!   `setSdkSessionConfig`, `resolveSdkSessionConfig`, `resolveSdkSettings`)
//! - `src/cli/ProjectRuntimeRegistry.ts:1807-2056` (`resolveSdkSessionPlugins`,
//!   `resolveSdkSessionSkills`, `resolveSdkConfiguredModel`,
//!   `assertSdkManagedModelAllowed`, `validateSdkManagedPermissions`,
//!   `validateSdkSelectorPolicy`, `applyOrganizationTaskBudgetCap`,
//!   `assertOrganizationSettingSourcesAllowed`, `mapPilotSdkSettingsError`,
//!   `hasOrganizationSdkSessionDefaults`, `isOrganizationModelAllowed`,
//!   `organizationProviderPolicyRejection`)
//! - `src/pilot/config/sdkSessionSettings.ts` (all 397 lines)
//!
//! ## Deliberate hangers (other slices)
//!
//! - `resolveSdkSubagentScopes` (TS `:2118-2204`) is outside this PR's cited
//!   ranges. `setSdkSessionConfig` therefore stores `agents` untouched.
//! - The plugin loader implementation is A6.3-pr7b. This module consumes
//!   [`SdkPluginLoader`], whose implementation is wired by that slice; the
//!   pr7 `PluginRuntime` supplies refresh/snapshot/createView.
//! - Session hook / SDK config-change **dispatch** is A6.7-pr6. This module
//!   defines the registration types and registry only (see
//!   [`SessionHandlerRegistry`]).
//! - Ephemeral-session storage deletion and MCP wiring are outside this file.
//!
//! ## JS semantics
//!
//! Property presence (`undefined` vs missing), `String(value)` and
//! `JSON.stringify` go through [`crate::js_value`] or the protocol's ordered
//! stringifier. No recursive traversal is written here; every walk is over
//! already-bounded configuration values.

use crate::{
    config::{Env, session_overrides::SessionConfigOverrides, session_value::SessionValue},
    js_value,
    metadata::resolve,
    runtime::plugins::{LoadedPlugin, PluginRuntime, PluginRuntimeError, PluginSkillContribution},
    storage::{TaskBudgetError, TaskBudgetStore},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use regex::Regex;
use std::{
    collections::HashSet,
    fmt,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
};

static UNDEFINED: JsValue = JsValue::Undefined;

// ─────────────────────────────────────────────────────────── JS helpers

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}

fn own<'a>(value: &'a JsValue, key: &str) -> Option<&'a JsValue> {
    value.as_object()?.get(key)
}

fn array(value: &JsValue) -> Option<&[JsValue]> {
    match value {
        JsValue::Array(items) => Some(items),
        _ => None,
    }
}

fn record(value: &JsValue) -> Option<&JsObject> {
    value.as_object()
}

fn is_record(value: &JsValue) -> bool {
    matches!(value, JsValue::Object(_))
}

fn nullish(value: &JsValue) -> bool {
    matches!(value, JsValue::Undefined | JsValue::Null)
}

fn is_null(value: &JsValue) -> bool {
    matches!(value, JsValue::Null)
}

/// ECMAScript `ToBoolean`.
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Object(_) | JsValue::Array(_) => true,
    }
}

/// `String.prototype.trim()` over UTF-16 units. The whitespace set is the one
/// `metadata::js_trim` uses for UTF-8; all members are BMP, so a per-unit test
/// is exact and preserves lone surrogates.
fn trim_units(value: &JsString) -> JsString {
    let is_ws = |unit: u16| {
        matches!(unit,
            0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a
            | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
    };
    let units = value.units();
    let start = units
        .iter()
        .position(|unit| !is_ws(*unit))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|unit| !is_ws(*unit))
        .map_or(start, |index| index + 1);
    JsString::from_utf16(units[start..end].to_vec())
}

fn number_text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

/// `${left}/${right}` built from UTF-16 units.
fn slash(parts: &[&JsString]) -> JsString {
    let mut units = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            units.push(u16::from(b'/'));
        }
        units.extend_from_slice(part.units());
    }
    JsString::from_utf16(units)
}

/// `Number.isInteger`.
fn is_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0
}

/// `Number.isSafeInteger`.
fn is_safe_integer(value: f64) -> bool {
    is_integer(value) && value.abs() <= 9_007_199_254_740_991.0
}

/// JavaScript `Math.min` (Rust's `f64::min` prefers the non-NaN operand).
fn math_min(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        return f64::NAN;
    }
    if left == right {
        if left == 0.0 {
            return if left.is_sign_negative() || right.is_sign_negative() {
                -0.0
            } else {
                0.0
            };
        }
        return left;
    }
    if left < right { left } else { right }
}

fn json_text(value: &JsValue) -> Option<String> {
    stringify_json(value).ok().flatten()
}

fn set_property(target: &mut JsValue, key: &str, value: JsValue) {
    if let JsValue::Object(object) = target {
        object.insert(key.into(), value);
    }
}

fn number(value: &JsValue) -> f64 {
    js_value::to_number(value).unwrap_or(f64::NAN)
}

fn path_js(path: &Path) -> JsString {
    JsString::from(path.to_string_lossy().into_owned())
}

fn js_object(entries: Vec<(impl Into<JsString>, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in entries {
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}

// ─────────────────────────────────────────────────────────── errors

/// The `DialogGatewayError` shape used by this slice: a stable wire code plus
/// the baseline's exact message text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SdkSessionError {
    pub code: String,
    pub message: String,
    /// `DialogGatewayError` for mapped gateway failures; the originating
    /// JavaScript error name for raw throws (`TypeError`, `Error`, ...).
    pub name: String,
}

impl SdkSessionError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            name: "DialogGatewayError".into(),
        }
    }
    /// A non-gateway throw: no `code`, and the baseline's error name.
    pub fn raw(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: String::new(),
            message: message.into(),
            name: name.into(),
        }
    }
    pub fn code(&self) -> &str {
        &self.code
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// The baseline's `PilotSdkSessionSettingsError`; the registry maps it to
/// `DialogGatewayError` via `mapPilotSdkSettingsError`.
fn settings_error(code: impl Into<String>, message: impl Into<String>) -> SdkSessionError {
    let mut error = SdkSessionError::new(code, message);
    error.name = "PilotSdkSessionSettingsError".into();
    error
}

fn map_settings_error(error: SdkSessionError) -> SdkSessionError {
    if error.name == "PilotSdkSessionSettingsError" {
        SdkSessionError::new(error.code, error.message)
    } else {
        error
    }
}

impl fmt::Display for SdkSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SdkSessionError {}

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
    for (key, _) in object.entries() {
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
    for (key, _) in agent.entries() {
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
        for (key, _) in thinking_object.entries() {
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
        for (key, _) in subagents_object.entries() {
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

fn home_dir(env: &Env) -> PathBuf {
    env.get("HOME")
        .map(PathBuf::from)
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
                "Unable to read {} settings source: {error}",
                source.as_str()
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
    let parsed = document.to_json().map_err(|error| {
        settings_error(
            "SDK_SETTING_SOURCE_YAML_INVALID",
            format!(
                "Unable to parse {} settings source: {}",
                source.as_str(),
                String::from_utf16_lossy(error.message.units())
            ),
        )
    })?;
    let parsed = parsed.to_js_value().map_err(|error| {
        settings_error(
            "SDK_SETTING_SOURCE_YAML_INVALID",
            format!(
                "Unable to parse {} settings source: {}",
                source.as_str(),
                String::from_utf16_lossy(error.message.units())
            ),
        )
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
    fn code(self) -> &'static str {
        match self {
            Self::Model => "INVALID_SDK_MODEL",
            Self::AgentModel => "INVALID_SDK_AGENT_MODEL",
            Self::FallbackModel => "INVALID_SDK_FALLBACK_MODEL",
            Self::SubagentModel => "INVALID_SDK_SUBAGENT_MODEL",
        }
    }
    fn label(self) -> &'static str {
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
    RE.get_or_init(|| Regex::new(r"^[^/\s]+\/(?:[^/\s]+|\*)$").unwrap())
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

// ─────────────────────────────────────────────────────────── runtime ports

/// One resolved project generation. The snapshot is the A6.3-pr2 shape; the
/// plugin runtime is the A6.3-pr7 owner.
pub struct ResolvedSdkRuntime {
    pub project_root: PathBuf,
    pub snapshot: JsValue,
    pub plugin_runtime: Arc<PluginRuntime>,
}

pub trait SdkRuntimeResolver: Send + Sync {
    fn resolve(
        &self,
        project_key: Option<&JsString>,
    ) -> Result<ResolvedSdkRuntime, SdkSessionError>;
}

pub type PluginLoadFuture<'a> =
    Pin<Box<dyn Future<Output = Result<LoadedPlugin, String>> + Send + 'a>>;

/// Path loader supplied by A6.3-pr7b. `resolveSdkSessionPlugins` owns the
/// realpath/duplicate/stat/name-conflict checks; only the manifest load is
/// delegated.
pub trait SdkPluginLoader: Send + Sync {
    fn load<'a>(&'a self, path: &'a Path) -> PluginLoadFuture<'a>;
}

/// Task-budget operations from A6.4-pr11. A6.3-pr8 decides *when* to call;
/// the store owns persistence and conflict text.
pub trait SdkTaskBudget: Send + Sync {
    fn configure_project(
        &self,
        project_root: &JsString,
        session_key: &JsString,
        total_usd: f64,
        retention_ms: Option<f64>,
        now_ms: f64,
    ) -> Result<(), TaskBudgetError>;
    fn clear_session(
        &self,
        project_root: &JsString,
        session_key: &JsString,
    ) -> Result<(), TaskBudgetError>;
}

/// Adapter over the merged [`TaskBudgetStore`].
pub struct TaskBudgetStorePort {
    store: Mutex<TaskBudgetStore>,
}

impl TaskBudgetStorePort {
    pub fn new(store: TaskBudgetStore) -> Self {
        Self {
            store: Mutex::new(store),
        }
    }
}

impl SdkTaskBudget for TaskBudgetStorePort {
    fn configure_project(
        &self,
        project_root: &JsString,
        session_key: &JsString,
        total_usd: f64,
        retention_ms: Option<f64>,
        now_ms: f64,
    ) -> Result<(), TaskBudgetError> {
        self.store
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .configure_project(project_root, session_key, total_usd, retention_ms, now_ms)
    }
    fn clear_session(
        &self,
        project_root: &JsString,
        session_key: &JsString,
    ) -> Result<(), TaskBudgetError> {
        self.store
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear_session(project_root, session_key)
    }
}

/// The subset of `AbortSignal` used by `setSdkSessionConfig`.
pub trait SdkAbortSignal {
    fn throw_if_aborted(&self) -> Result<(), SdkSessionError>;
}

fn check_abort(signal: Option<&dyn SdkAbortSignal>) -> Result<(), SdkSessionError> {
    match signal {
        Some(signal) => signal.throw_if_aborted(),
        None => Ok(()),
    }
}

// ─────────────────────────────────────────────────────────── handler registry

/// `PilotDeckHookEvent` from the baseline's hook protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PilotDeckHookEvent {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    Notification,
    UserPromptSubmit,
    PreModelRequest,
    SessionStart,
    SessionEnd,
    Stop,
    StopFailure,
    SubagentStart,
    SubagentStop,
    PreCompact,
    PostCompact,
    PermissionRequest,
    PermissionDenied,
    Setup,
    ConfigChange,
    InstructionsLoaded,
    CwdChanged,
    FileChanged,
    WorktreeCreate,
    WorktreeRemove,
    Elicitation,
    ElicitationResult,
}

impl PilotDeckHookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::Notification => "Notification",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreModelRequest => "PreModelRequest",
            Self::SessionStart => "SessionStart",
            Self::SessionEnd => "SessionEnd",
            Self::Stop => "Stop",
            Self::StopFailure => "StopFailure",
            Self::SubagentStart => "SubagentStart",
            Self::SubagentStop => "SubagentStop",
            Self::PreCompact => "PreCompact",
            Self::PostCompact => "PostCompact",
            Self::PermissionRequest => "PermissionRequest",
            Self::PermissionDenied => "PermissionDenied",
            Self::Setup => "Setup",
            Self::ConfigChange => "ConfigChange",
            Self::InstructionsLoaded => "InstructionsLoaded",
            Self::CwdChanged => "CwdChanged",
            Self::FileChanged => "FileChanged",
            Self::WorktreeCreate => "WorktreeCreate",
            Self::WorktreeRemove => "WorktreeRemove",
            Self::Elicitation => "Elicitation",
            Self::ElicitationResult => "ElicitationResult",
        }
    }
}

/// SDK config-change payload type (dispatch belongs to A6.7-pr6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SdkConfigChangePayload {
    pub changed_paths: Vec<String>,
    pub change_classes: Vec<String>,
}

pub type SdkConfigChangeHandler = Arc<dyn Fn(&SdkConfigChangePayload) + Send + Sync>;
pub type SessionHookHandler = Arc<dyn Fn(PilotDeckHookEvent, &JsValue) + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandlerRegistration {
    session_key: JsString,
    id: u64,
}

struct ConfigChangeRegistration {
    id: u64,
    handler: SdkConfigChangeHandler,
}

struct SessionHookRegistration {
    id: u64,
    handler: SessionHookHandler,
}

/// Registration-only registry. The TS `dispatchSessionHook` and
/// `dispatchSdkConfigChange` loops are intentionally **not** implemented here;
/// A6.7-pr6 owns dispatch. Entries keep JS Map semantics: one registration per
/// session, later registration replaces it in place, and an old token cannot
/// remove a newer registration.
#[derive(Default)]
pub struct SessionHandlerRegistry {
    config_change: Vec<(JsString, ConfigChangeRegistration)>,
    session_hook: Vec<(JsString, SessionHookRegistration)>,
    next_id: u64,
}

impl SessionHandlerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn take_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn register_config_change(
        &mut self,
        session_key: &JsString,
        handler: SdkConfigChangeHandler,
    ) -> HandlerRegistration {
        let id = self.take_id();
        let entry = ConfigChangeRegistration { id, handler };
        match self
            .config_change
            .iter_mut()
            .find(|(key, _)| key == session_key)
        {
            Some((_, current)) => *current = entry,
            None => self.config_change.push((session_key.clone(), entry)),
        }
        HandlerRegistration {
            session_key: session_key.clone(),
            id,
        }
    }

    pub fn register_session_hook(
        &mut self,
        session_key: &JsString,
        handler: SessionHookHandler,
    ) -> HandlerRegistration {
        let id = self.take_id();
        let entry = SessionHookRegistration { id, handler };
        match self
            .session_hook
            .iter_mut()
            .find(|(key, _)| key == session_key)
        {
            Some((_, current)) => *current = entry,
            None => self.session_hook.push((session_key.clone(), entry)),
        }
        HandlerRegistration {
            session_key: session_key.clone(),
            id,
        }
    }

    /// The TS unregister closure only deletes when the stored registration is
    /// the same object; a stale token is a no-op.
    pub fn unregister_config_change(&mut self, registration: &HandlerRegistration) {
        self.config_change.retain(|(key, current)| {
            !(key == &registration.session_key && current.id == registration.id)
        });
    }

    pub fn unregister_session_hook(&mut self, registration: &HandlerRegistration) {
        self.session_hook.retain(|(key, current)| {
            !(key == &registration.session_key && current.id == registration.id)
        });
    }

    pub fn has_config_change(&self, session_key: &JsString) -> bool {
        self.config_change.iter().any(|(key, _)| key == session_key)
    }

    pub fn has_session_hook(&self, session_key: &JsString) -> bool {
        self.session_hook.iter().any(|(key, _)| key == session_key)
    }

    /// The registered handler, exposed for A6.7-pr6 dispatch.
    pub fn config_change_handler(&self, session_key: &JsString) -> Option<&SdkConfigChangeHandler> {
        self.config_change
            .iter()
            .find(|(key, _)| key == session_key)
            .map(|(_, entry)| &entry.handler)
    }

    /// The registered handler, exposed for A6.7-pr6 dispatch.
    pub fn session_hook_handler(&self, session_key: &JsString) -> Option<&SessionHookHandler> {
        self.session_hook
            .iter()
            .find(|(key, _)| key == session_key)
            .map(|(_, entry)| &entry.handler)
    }

    pub fn config_change_sessions(&self) -> Vec<JsString> {
        self.config_change
            .iter()
            .map(|(key, _)| key.clone())
            .collect()
    }

    pub fn session_hook_sessions(&self) -> Vec<JsString> {
        self.session_hook
            .iter()
            .map(|(key, _)| key.clone())
            .collect()
    }

    pub fn clear_session(&mut self, session_key: &JsString) {
        self.config_change.retain(|(key, _)| key != session_key);
        self.session_hook.retain(|(key, _)| key != session_key);
    }

    pub fn clear(&mut self) {
        self.config_change.clear();
        self.session_hook.clear();
    }
}

// ─────────────────────────────────────────────────────────── registry

pub struct SdkSessionOptions {
    pub fallback_project_root: PathBuf,
    pub cwd: PathBuf,
    pub env: Env,
    pub resolver: Arc<dyn SdkRuntimeResolver>,
    pub organization_policy: Option<OrganizationPolicy>,
    pub plugin_loader: Option<Arc<dyn SdkPluginLoader>>,
    pub task_budget: Option<Arc<dyn SdkTaskBudget>>,
    pub clock: Arc<dyn Fn() -> f64 + Send + Sync>,
}

impl SdkSessionOptions {
    pub fn new(
        fallback_project_root: impl Into<PathBuf>,
        cwd: impl Into<PathBuf>,
        env: Env,
        resolver: Arc<dyn SdkRuntimeResolver>,
    ) -> Self {
        Self {
            fallback_project_root: fallback_project_root.into(),
            cwd: cwd.into(),
            env,
            resolver,
            organization_policy: None,
            plugin_loader: None,
            task_budget: None,
            clock: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis() as f64)
                    .unwrap_or(0.0)
            }),
        }
    }
    pub fn with_organization_policy(mut self, policy: Option<OrganizationPolicy>) -> Self {
        self.organization_policy = policy;
        self
    }
    pub fn with_plugin_loader(mut self, loader: Arc<dyn SdkPluginLoader>) -> Self {
        self.plugin_loader = Some(loader);
        self
    }
    pub fn with_task_budget(mut self, budget: Arc<dyn SdkTaskBudget>) -> Self {
        self.task_budget = Some(budget);
        self
    }
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> f64 + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlagSettingsResult {
    pub applied: Vec<&'static str>,
    pub cleared: Vec<&'static str>,
}

pub struct SdkSessionRegistry {
    options: SdkSessionOptions,
    configs: Vec<(JsString, JsValue)>,
    plugins: Vec<(JsString, Vec<Arc<LoadedPlugin>>)>,
    thinking: Vec<(JsString, JsValue)>,
    projects: Vec<(JsString, PathBuf)>,
    overrides: Option<SessionConfigOverrides>,
    handlers: SessionHandlerRegistry,
}

impl SdkSessionRegistry {
    pub fn new(options: SdkSessionOptions) -> Self {
        Self {
            options,
            configs: Vec::new(),
            plugins: Vec::new(),
            thinking: Vec::new(),
            projects: Vec::new(),
            overrides: None,
            handlers: SessionHandlerRegistry::new(),
        }
    }

    pub fn with_overrides(mut self, overrides: SessionConfigOverrides) -> Self {
        self.overrides = Some(overrides);
        self
    }

    pub fn options(&self) -> &SdkSessionOptions {
        &self.options
    }

    pub fn handlers(&self) -> &SessionHandlerRegistry {
        &self.handlers
    }

    pub fn handlers_mut(&mut self) -> &mut SessionHandlerRegistry {
        &mut self.handlers
    }

    /// Full configuration insertion order, for fixture replay and inspection.
    pub fn config_entries(&self) -> &[(JsString, JsValue)] {
        &self.configs
    }
    pub fn plugin_entries(&self) -> &[(JsString, Vec<Arc<LoadedPlugin>>)] {
        &self.plugins
    }
    pub fn thinking_entries(&self) -> &[(JsString, JsValue)] {
        &self.thinking
    }
    pub fn project_entries(&self) -> &[(JsString, PathBuf)] {
        &self.projects
    }

    pub fn session_config(&self, session_key: &JsString) -> Option<&JsValue> {
        self.configs
            .iter()
            .find(|(key, _)| key == session_key)
            .map(|(_, value)| value)
    }

    pub fn session_plugins(&self, session_key: &JsString) -> &[Arc<LoadedPlugin>] {
        self.plugins
            .iter()
            .find(|(key, _)| key == session_key)
            .map(|(_, plugins)| plugins.as_slice())
            .unwrap_or(&[])
    }

    pub fn session_project(&self, session_key: &JsString) -> Option<&Path> {
        self.projects
            .iter()
            .find(|(key, _)| key == session_key)
            .map(|(_, root)| root.as_path())
    }

    /// `None` means no override entry; `Some(Null)` is an explicit `null`.
    pub fn thinking_override(&self, session_key: &JsString) -> Option<&JsValue> {
        self.thinking
            .iter()
            .find(|(key, _)| key == session_key)
            .map(|(_, value)| value)
    }

    pub fn session_overrides(&self) -> Option<&SessionConfigOverrides> {
        self.overrides.as_ref()
    }

    fn set_config(&mut self, session_key: &JsString, config: JsValue) {
        match self.configs.iter_mut().find(|(key, _)| key == session_key) {
            Some((_, value)) => *value = config,
            None => self.configs.push((session_key.clone(), config)),
        }
    }

    fn delete_config(&mut self, session_key: &JsString) {
        self.configs.retain(|(key, _)| key != session_key);
    }

    fn set_plugins(&mut self, session_key: &JsString, plugins: Vec<Arc<LoadedPlugin>>) {
        match self.plugins.iter_mut().find(|(key, _)| key == session_key) {
            Some((_, value)) => *value = plugins,
            None => self.plugins.push((session_key.clone(), plugins)),
        }
    }

    fn delete_plugins(&mut self, session_key: &JsString) {
        self.plugins.retain(|(key, _)| key != session_key);
    }

    fn set_thinking(&mut self, session_key: &JsString, value: JsValue) {
        match self.thinking.iter_mut().find(|(key, _)| key == session_key) {
            Some((_, current)) => *current = value,
            None => self.thinking.push((session_key.clone(), value)),
        }
    }

    fn delete_thinking(&mut self, session_key: &JsString) {
        self.thinking.retain(|(key, _)| key != session_key);
    }

    fn set_project(&mut self, session_key: &JsString, root: PathBuf) {
        match self.projects.iter_mut().find(|(key, _)| key == session_key) {
            Some((_, current)) => *current = root,
            None => self.projects.push((session_key.clone(), root)),
        }
    }

    fn delete_project(&mut self, session_key: &JsString) {
        self.projects.retain(|(key, _)| key != session_key);
    }

    fn resolve_settings(
        &self,
        runtime: &ResolvedSdkRuntime,
        config: &JsValue,
    ) -> Result<Option<JsValue>, SdkSessionError> {
        let settings = self
            .options
            .organization_policy
            .as_ref()
            .and_then(|policy| policy.settings.as_ref());
        resolve_session_settings(ResolveSettingsOptions {
            project_root: &runtime.project_root,
            cwd: &self.options.cwd,
            env: &self.options.env,
            host_defaults: settings.and_then(|value| value.session_defaults.as_ref()),
            host_managed_settings: settings
                .and_then(|value| value.managed_session_settings.as_ref()),
            host_setting_sources: settings.and_then(|value| value.session_default_sources.as_ref()),
            settings: own(config, "settings"),
            setting_sources: own(config, "settingSources"),
            host_enforced_settings: settings
                .and_then(|value| value.enforced_session_settings.as_ref()),
        })
        .map_err(map_settings_error)
    }

    fn assert_organization_model_allowed(
        &self,
        runtime: &ResolvedSdkRuntime,
        model: &ModelRef,
    ) -> Result<(), SdkSessionError> {
        let policy = self.options.organization_policy.as_ref();
        if !is_organization_model_allowed(policy.and_then(|policy| policy.models.as_ref()), model) {
            return Err(SdkSessionError::new(
                "GATEWAY_ORGANIZATION_MODEL_DENIED",
                format!(
                    "Gateway organization policy denies model {}/{}.",
                    number_text(&model.provider),
                    number_text(&model.model)
                ),
            ));
        }
        let provider = field(field(&runtime.snapshot, "config"), "model")
            .get("providers")
            .and_then(|providers| providers.get(&number_text(&model.provider)));
        if let Some(rejection) = organization_provider_policy_rejection(
            policy.and_then(|policy| policy.providers.as_ref()),
            &model.provider,
            provider,
        ) {
            return Err(rejection);
        }
        Ok(())
    }

    /// `resolveSdkSessionConfig`; the settings key is always present (as
    /// `undefined` when no overlay resolves), matching the TS object spread.
    pub fn resolve_session_config(
        &self,
        session_key: &JsString,
        runtime: &ResolvedSdkRuntime,
    ) -> Result<Option<JsValue>, SdkSessionError> {
        let Some(stored) = self.session_config(session_key) else {
            return Ok(None);
        };
        let settings = self.resolve_settings(runtime, stored)?;
        let mut resolved = stored.clone();
        set_property(
            &mut resolved,
            "settings",
            settings.unwrap_or(JsValue::Undefined),
        );
        Ok(Some(resolved))
    }

    /// `assertSdkModelAllowed`.
    pub fn assert_model_allowed(
        &self,
        session_key: &JsString,
        model: Option<&ModelRef>,
        project_key: Option<&JsString>,
    ) -> Result<(), SdkSessionError> {
        let project_root = match project_key {
            Some(key) => key.clone(),
            None => match self.session_project(session_key) {
                Some(root) => path_js(root),
                None => path_js(&self.options.fallback_project_root),
            },
        };
        let runtime = self.options.resolver.resolve(Some(&project_root))?;
        let resolved = match model {
            Some(model) => model.clone(),
            None => {
                let requested = self
                    .resolve_session_config(session_key, &runtime)?
                    .and_then(|config| {
                        field(&config, "settings")
                            .get("agent")
                            .and_then(|agent| agent.get("model"))
                            .cloned()
                    });
                match requested {
                    Some(value) if truthy(&value) => {
                        let Some(requested) = value.as_string() else {
                            return Err(SdkSessionError::new(
                                "INVALID_SDK_AGENT_MODEL",
                                format!(
                                    "settings.agent.model is not uniquely resolvable from the Gateway catalog: {}",
                                    number_text(&JsString::from(String::from_utf16_lossy(
                                        js_value::to_string_units(&value)
                                            .unwrap_or_default()
                                            .as_slice()
                                    )))
                                ),
                            ));
                        };
                        resolve_configured_model(
                            &runtime.snapshot,
                            requested,
                            ModelField::AgentModel,
                        )?
                    }
                    _ => {
                        let configured =
                            field(field(&runtime.snapshot, "config"), "agent").get("model");
                        let provider = configured
                            .and_then(|model| model.get("provider"))
                            .and_then(JsValue::as_string)
                            .cloned()
                            .unwrap_or_default();
                        let name = configured
                            .and_then(|model| model.get("model"))
                            .and_then(JsValue::as_string)
                            .cloned()
                            .unwrap_or_default();
                        ModelRef {
                            provider,
                            model: name,
                        }
                    }
                }
            }
        };
        self.assert_organization_model_allowed(&runtime, &resolved)?;
        assert_managed_model_allowed(
            self.session_config(session_key)
                .and_then(|config| own(config, "managedModels")),
            &resolved,
        )
    }

    /// `setSessionThinkingForSdk`: `None` deletes the override, `Some(Null)`
    /// stores an explicit `null`, and objects are cloned.
    pub fn set_session_thinking(&mut self, session_key: &JsString, thinking: Option<&JsValue>) {
        match thinking {
            None => self.delete_thinking(session_key),
            Some(value) => self.set_thinking(session_key, value.clone()),
        }
    }

    /// `applyFlagSettingsForSdk`.
    pub fn apply_flag_settings(
        &mut self,
        session_key: &JsString,
        settings: &JsValue,
    ) -> Result<FlagSettingsResult, SdkSessionError> {
        let mut result = FlagSettingsResult::default();
        let has = |key: &str| {
            settings
                .as_object()
                .is_some_and(|object| object.contains_key(key))
        };
        if has("effortLevel") {
            let effort = field(settings, "effortLevel");
            if is_null(effort) {
                self.delete_thinking(session_key);
                result.cleared.push("effortLevel");
            } else {
                self.set_thinking(
                    session_key,
                    js_object(vec![
                        ("enabled", JsValue::Bool(true)),
                        ("mode", effort.clone()),
                    ]),
                );
                result.applied.push("effortLevel");
            }
        }
        if has("permissions") {
            let permissions = field(settings, "permissions");
            if permissions.is_undefined() {
                return Err(SdkSessionError::raw(
                    "TypeError",
                    "Cannot read properties of undefined (reading 'defaultMode')",
                ));
            }
            let default_mode = if is_null(permissions) {
                JsValue::Null
            } else {
                field(permissions, "defaultMode").clone()
            };
            if is_null(&default_mode) || default_mode.is_undefined() {
                if let Some(overrides) = &mut self.overrides
                    && let Some(existing) = overrides.get(session_key)
                    && existing
                        .get("permissionMode")
                        .is_some_and(|value| !value.to_value().is_undefined())
                {
                    let value = existing.to_value();
                    if let JsValue::Object(object) = &value {
                        let mut next = JsObject::new();
                        for (key, value) in object.entries() {
                            if !key.equals_str("permissionMode") {
                                next.insert(key.clone(), value.clone());
                            }
                        }
                        overrides.set(session_key, &SessionValue::from(JsValue::Object(next)));
                    }
                }
                result.cleared.push("permissions.defaultMode");
            } else {
                if let Some(overrides) = &mut self.overrides {
                    let existing = overrides
                        .get(session_key)
                        .unwrap_or_else(SessionValue::object);
                    let mut value = existing.to_value();
                    if let JsValue::Object(object) = &mut value {
                        object.insert("permissionMode".into(), default_mode);
                    }
                    overrides.set(session_key, &SessionValue::from(value));
                }
                result.applied.push("permissions.defaultMode");
            }
        }
        Ok(result)
    }

    pub fn register_config_change_handler(
        &mut self,
        session_key: &JsString,
        handler: SdkConfigChangeHandler,
    ) -> HandlerRegistration {
        self.handlers.register_config_change(session_key, handler)
    }

    pub fn register_session_hook_handler(
        &mut self,
        session_key: &JsString,
        handler: SessionHookHandler,
    ) -> HandlerRegistration {
        self.handlers.register_session_hook(session_key, handler)
    }

    pub fn is_ephemeral_session(&self, session_key: &JsString) -> bool {
        self.session_config(session_key)
            .and_then(|config| own(config, "persistSession"))
            .is_some_and(|value| matches!(value, JsValue::Bool(false)))
    }

    /// `clearSdkSessionState`. MCP fields are owned by A6.5 and not modeled
    /// here; the task-budget clear is called only when a project was recorded.
    pub fn clear_session_state(&mut self, session_key: &JsString) -> Result<(), SdkSessionError> {
        if let Some(root) = self.session_project(session_key).map(Path::to_path_buf)
            && let Some(budget) = &self.options.task_budget
        {
            budget
                .clear_session(&path_js(&root), session_key)
                .map_err(task_budget_error)?;
        }
        self.delete_config(session_key);
        self.delete_plugins(session_key);
        self.delete_thinking(session_key);
        self.delete_project(session_key);
        self.handlers.clear_session(session_key);
        Ok(())
    }

    async fn resolve_session_plugins(
        &self,
        requested: Option<&JsValue>,
        runtime: &ResolvedSdkRuntime,
    ) -> Result<Vec<Arc<LoadedPlugin>>, SdkSessionError> {
        let Some(requested) = requested else {
            return Ok(Vec::new());
        };
        let Some(descriptors) = array(requested) else {
            return Ok(Vec::new());
        };
        if descriptors.is_empty() {
            return Ok(Vec::new());
        }
        let mut names: Vec<String> = runtime
            .plugin_runtime
            .snapshot()
            .await
            .iter()
            .map(|plugin| plugin.name.clone())
            .collect();
        let mut paths: Vec<PathBuf> = Vec::new();
        let mut plugins = Vec::new();
        for descriptor in descriptors {
            let requested_path = field(descriptor, "path");
            let Some(requested_text) = requested_path.as_string() else {
                return Err(SdkSessionError::new(
                    "SDK_PLUGIN_NOT_FOUND",
                    format!(
                        "Gateway-local SDK plugin path does not exist: {}",
                        display_value(requested_path)
                    ),
                ));
            };
            let canonical = std::fs::canonicalize(Path::new(&number_text(requested_text)))
                .map_err(|_| {
                    SdkSessionError::new(
                        "SDK_PLUGIN_NOT_FOUND",
                        format!(
                            "Gateway-local SDK plugin path does not exist: {}",
                            number_text(requested_text)
                        ),
                    )
                })?;
            if paths.contains(&canonical) {
                return Err(SdkSessionError::new(
                    "SDK_PLUGIN_DUPLICATE_PATH",
                    format!(
                        "Gateway-local SDK plugin path is listed more than once: {}",
                        number_text(requested_text)
                    ),
                ));
            }
            paths.push(canonical.clone());
            let metadata = std::fs::metadata(&canonical).map_err(|_| {
                SdkSessionError::new(
                    "SDK_PLUGIN_NOT_FOUND",
                    format!(
                        "Gateway-local SDK plugin path does not exist: {}",
                        number_text(requested_text)
                    ),
                )
            })?;
            if !metadata.is_dir() {
                return Err(SdkSessionError::new(
                    "SDK_PLUGIN_NOT_DIRECTORY",
                    format!(
                        "Gateway-local SDK plugin path is not a directory: {}",
                        number_text(requested_text)
                    ),
                ));
            }
            let Some(loader) = &self.options.plugin_loader else {
                return Err(SdkSessionError::new(
                    "SDK_PLUGIN_LOAD_FAILED",
                    format!(
                        "Could not load SDK plugin {}: plugin loading is not available.",
                        number_text(requested_text)
                    ),
                ));
            };
            let plugin = loader.load(&canonical).await.map_err(|error| {
                SdkSessionError::new(
                    "SDK_PLUGIN_LOAD_FAILED",
                    format!(
                        "Could not load SDK plugin {}: {error}",
                        number_text(requested_text)
                    ),
                )
            })?;
            if names.iter().any(|name| name == &plugin.name) {
                return Err(SdkSessionError::new(
                    "SDK_PLUGIN_NAME_CONFLICT",
                    format!(
                        "SDK plugin {} conflicts with an already discovered project plugin.",
                        plugin.name
                    ),
                ));
            }
            names.push(plugin.name.clone());
            plugins.push(Arc::new(plugin));
        }
        Ok(plugins)
    }

    fn configure_project_task_budget(
        &self,
        project_root: &Path,
        session_key: &JsString,
        task_budget: Option<&JsValue>,
        signal: Option<&dyn SdkAbortSignal>,
    ) -> Result<(), SdkSessionError> {
        let Some(task_budget) = task_budget else {
            return Ok(());
        };
        if !field(task_budget, "scope")
            .as_string()
            .is_some_and(|scope| scope.equals_str("project"))
        {
            return Ok(());
        }
        let Some(budget) = &self.options.task_budget else {
            return Err(SdkSessionError::new(
                "gateway_request_failed",
                "SDK task budget storage is not configured.",
            ));
        };
        check_abort(signal)?;
        let total = number(field(task_budget, "total"));
        let retention = own(task_budget, "projectRetentionMs")
            .filter(|value| !value.is_undefined())
            .map(number);
        budget
            .configure_project(
                &path_js(project_root),
                session_key,
                total,
                retention,
                (self.options.clock)(),
            )
            .map_err(task_budget_error)
    }

    /// `setSdkSessionConfig`. Validation and every fail-fast check run before
    /// any state mutation, exactly as in the baseline.
    pub async fn set_session_config(
        &mut self,
        session_key: &JsString,
        config: &JsValue,
        project_key: Option<&JsString>,
        signal: Option<&dyn SdkAbortSignal>,
    ) -> Result<bool, SdkSessionError> {
        check_abort(signal)?;
        let mut next = config.clone();
        apply_organization_task_budget_cap(
            &mut next,
            self.options
                .organization_policy
                .as_ref()
                .and_then(|policy| policy.max_task_budget_usd),
        );
        validate_managed_permissions(own(&next, "managedPermissions"))?;
        validate_selector_policy(own(&next, "managedTools"), SelectorKind::Tools)?;
        validate_selector_policy(own(&next, "managedModels"), SelectorKind::Models)?;
        validate_setting_sources(own(&next, "settingSources")).map_err(map_settings_error)?;
        assert_organization_setting_sources_allowed(
            self.options
                .organization_policy
                .as_ref()
                .and_then(|policy| policy.setting_sources.as_ref()),
            own(&next, "settingSources"),
        )?;
        let previous = self.session_config(session_key).cloned();
        let runtime = self.options.resolver.resolve(project_key)?;
        let resolved_settings = self.resolve_settings(&runtime, &next)?;
        for (requested, field) in [
            (
                resolved_settings
                    .as_ref()
                    .and_then(|settings| settings.get("agent"))
                    .and_then(|agent| agent.get("model")),
                ModelField::AgentModel,
            ),
            (
                resolved_settings
                    .as_ref()
                    .and_then(|settings| settings.get("agent"))
                    .and_then(|agent| agent.get("fallbackModel")),
                ModelField::FallbackModel,
            ),
            (
                resolved_settings
                    .as_ref()
                    .and_then(|settings| settings.get("agent"))
                    .and_then(|agent| agent.get("subagents"))
                    .and_then(|subagents| subagents.get("default")),
                ModelField::SubagentModel,
            ),
        ] {
            let Some(requested) = requested else {
                continue;
            };
            if nullish(requested) {
                continue;
            }
            let model = resolve_configured_model_value(&runtime.snapshot, requested, field)?;
            self.assert_organization_model_allowed(&runtime, &model)?;
            assert_managed_model_allowed(own(&next, "managedModels"), &model)?;
        }

        let has_host_defaults = self
            .options
            .organization_policy
            .as_ref()
            .is_some_and(OrganizationPolicy::has_session_defaults);
        let same_json = match &previous {
            Some(previous) => json_text(previous) == json_text(&next),
            None => json_text(&next).is_none(),
        };
        if same_json && !(has_host_defaults && previous.is_none()) {
            return Ok(false);
        }

        runtime
            .plugin_runtime
            .refresh()
            .await
            .map_err(plugin_error)?;
        check_abort(signal)?;
        let session_plugins = self
            .resolve_session_plugins(own(&next, "plugins"), &runtime)
            .await?;
        let view = runtime
            .plugin_runtime
            .create_view(session_plugins.clone())
            .await;
        if let Some(skills) =
            own(&next, "skills").filter(|value| matches!(value, JsValue::Array(_)))
        {
            let available = view.all_skills();
            let items = array(skills).unwrap_or(&[]);
            let resolved = resolve_session_skills(items, &available)?;
            set_property(&mut next, "skills", resolved);
        }
        // resolveSdkSubagentScopes (TS :2118) is outside this slice's ranges.

        if let Some(model) = own(&next, "settings")
            .and_then(|settings| settings.get("agent"))
            .and_then(|agent| agent.get("model"))
            .filter(|value| truthy(value))
        {
            let resolved =
                resolve_configured_model_value(&runtime.snapshot, model, ModelField::Model)?;
            self.assert_organization_model_allowed(&runtime, &resolved)?;
            assert_managed_model_allowed(own(&next, "managedModels"), &resolved)?;
        }
        if let Some(fallback) = own(&next, "fallbackModel").filter(|value| truthy(value)) {
            let resolved = resolve_configured_model_value(
                &runtime.snapshot,
                fallback,
                ModelField::FallbackModel,
            )?;
            self.assert_organization_model_allowed(&runtime, &resolved)?;
            assert_managed_model_allowed(own(&next, "managedModels"), &resolved)?;
        }

        self.configure_project_task_budget(
            &runtime.project_root,
            session_key,
            own(&next, "taskBudget"),
            signal,
        )?;
        check_abort(signal)?;
        self.set_config(session_key, next.clone());
        if session_plugins.is_empty() {
            self.delete_plugins(session_key);
        } else {
            self.set_plugins(session_key, session_plugins);
        }
        self.set_project(session_key, runtime.project_root);
        Ok(true)
    }
}

fn resolve_configured_model_value(
    snapshot: &JsValue,
    requested: &JsValue,
    model_field: ModelField,
) -> Result<ModelRef, SdkSessionError> {
    match requested.as_string() {
        Some(requested) => resolve_configured_model(snapshot, requested, model_field),
        None => Err(SdkSessionError::new(
            model_field.code(),
            format!(
                "{} is not uniquely resolvable from the Gateway catalog: {}",
                model_field.label(),
                display_value(requested)
            ),
        )),
    }
}

fn display_value(value: &JsValue) -> String {
    js_value::display(value).unwrap_or_else(|_| "Maximum call stack size exceeded".into())
}

pub fn resolve_session_skills(
    requested: &[JsValue],
    available: &[PluginSkillContribution],
) -> Result<JsValue, SdkSessionError> {
    let mut resolved: Vec<String> = Vec::new();
    for requested_name in requested {
        let requested_text = display_value(requested_name);
        let exact: Vec<&PluginSkillContribution> = available
            .iter()
            .filter(|skill| skill.name == requested_text)
            .collect();
        let candidates: Vec<&PluginSkillContribution> = if exact.is_empty() {
            let suffix = format!(":{requested_text}");
            available
                .iter()
                .filter(|skill| skill.name.ends_with(&suffix))
                .collect()
        } else {
            exact
        };
        if candidates.is_empty() {
            return Err(SdkSessionError::new(
                "SDK_SKILL_NOT_FOUND",
                format!("SDK session skill is not available in this project: {requested_text}"),
            ));
        }
        if candidates.len() > 1 {
            return Err(SdkSessionError::new(
                "SDK_SKILL_AMBIGUOUS",
                format!(
                    "SDK session skill is ambiguous; use its fully-qualified name: {requested_text}"
                ),
            ));
        }
        let name = candidates[0].name.clone();
        if !resolved.contains(&name) {
            resolved.push(name);
        }
    }
    Ok(JsValue::Array(
        resolved
            .into_iter()
            .map(|name| JsValue::String(JsString::from(name)))
            .collect(),
    ))
}

fn task_budget_error(error: TaskBudgetError) -> SdkSessionError {
    match error.code() {
        Some(code) => SdkSessionError::new(code, error.to_string()),
        None => SdkSessionError::raw("Error", error.to_string()),
    }
}

fn plugin_error(error: PluginRuntimeError) -> SdkSessionError {
    SdkSessionError::raw("Error", error.to_string())
}

#[cfg(test)]
#[path = "sdk_session_tests.rs"]
mod tests;
