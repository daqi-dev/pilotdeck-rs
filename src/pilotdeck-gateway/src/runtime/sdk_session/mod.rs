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

use crate::js_value;
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{fmt, path::Path};

mod handlers;
mod policy;
mod registry;
mod settings;

pub use handlers::*;
pub use policy::*;
pub use registry::*;
pub use settings::*;

#[cfg(test)]
mod tests;

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

/// The three primitive `typeof` labels a session config can carry. Objects and
/// arrays are assigned properties normally, so they return `None`.
fn primitive_type(value: &JsValue) -> Option<&'static str> {
    match value {
        JsValue::Number(_) => Some("number"),
        JsValue::String(_) => Some("string"),
        JsValue::Bool(_) => Some("boolean"),
        _ => None,
    }
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
