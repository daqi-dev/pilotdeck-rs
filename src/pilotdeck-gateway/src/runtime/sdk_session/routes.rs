//! Shared CONFIG session validation; mutations live in SdkSessionRegistry.
use super::field;
use crate::config::routes::{Result, RouteError};
use pilotdeck_protocol::{JsString, JsValue};
pub(crate) fn session_key(params: &JsValue) -> Result<JsString> {
    match field(params, "sessionKey") {
        JsValue::Undefined | JsValue::Null => Err(RouteError::new(
            "INVALID_SESSION_KEY",
            "sessionKey is required.",
        )),
        JsValue::String(key) if !super::trim_units(key).is_empty() => Ok(key.clone()),
        JsValue::String(_) => Err(RouteError::new(
            "INVALID_SESSION_KEY",
            "sessionKey is required.",
        )),
        _ => Err(RouteError::new(
            "gateway_request_failed",
            "input.sessionKey?.trim is not a function",
        )),
    }
}
pub(crate) fn permission_mode(value: &JsValue) -> bool {
    value.as_string().is_some_and(|s| {
        ["default", "plan", "bypassPermissions"]
            .iter()
            .any(|v| s.equals_str(v))
    })
}
pub(crate) fn validate_flags(settings: &JsValue) -> Result<()> {
    let obj = settings
        .as_object()
        .ok_or_else(|| RouteError::new("INVALID_FLAG_SETTINGS", "settings must be an object."))?;
    for (key, value) in obj.entries() {
        if key.equals_str("effortLevel") {
            if !matches!(value, JsValue::Null)
                && !value
                    .as_string()
                    .is_some_and(|s| ["low", "medium", "high"].iter().any(|v| s.equals_str(v)))
            {
                return Err(RouteError::new(
                    "INVALID_FLAG_SETTINGS",
                    "effortLevel must be low, medium, high, or null.",
                ));
            }
        } else if key.equals_str("permissions") {
            if *value == JsValue::Null {
                continue;
            }
            let perms = value.as_object().ok_or_else(|| {
                RouteError::new(
                    "INVALID_FLAG_SETTINGS",
                    "permissions must be an object or null.",
                )
            })?;
            for (k, v) in perms.entries() {
                if !k.equals_str("defaultMode") {
                    return Err(RouteError::new(
                        "UNSUPPORTED_FLAG_SETTING",
                        format!(
                            "permissions.{} has no PilotDeck session equivalent.",
                            String::from_utf16_lossy(k.units())
                        ),
                    ));
                }
                if *v != JsValue::Null && !permission_mode(v) {
                    return Err(RouteError::new(
                        "UNSUPPORTED_FLAG_SETTING",
                        format!(
                            "permissions.defaultMode={} is not supported.",
                            String::from_utf16_lossy(
                                &crate::js_value::to_string_units(v).unwrap_or_default()
                            )
                        ),
                    ));
                }
            }
        } else {
            return Err(RouteError::new(
                "UNSUPPORTED_FLAG_SETTING",
                format!(
                    "{} has no PilotDeck session equivalent.",
                    String::from_utf16_lossy(key.units())
                ),
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_thinking(thinking: &JsValue) -> Result<()> {
    if *thinking == JsValue::Null {
        return Ok(());
    }
    if !matches!(thinking, JsValue::Object(_) | JsValue::Array(_))
        || field(thinking, "enabled").as_bool().is_none()
    {
        return Err(RouteError::new(
            "INVALID_THINKING_CONFIG",
            "thinking must be null or an object with an enabled boolean.",
        ));
    }
    let mode = field(thinking, "mode");
    if !mode.is_undefined()
        && !mode.as_string().is_some_and(|s| {
            [
                "default", "off", "minimal", "low", "medium", "high", "xhigh", "max",
            ]
            .iter()
            .any(|v| s.equals_str(v))
        })
    {
        return Err(RouteError::new(
            "INVALID_THINKING_CONFIG",
            format!(
                "Unsupported thinking mode: {}.",
                crate::js_value::display(mode).unwrap_or_default()
            ),
        ));
    }
    let budget = field(thinking, "budgetTokens");
    if !budget.is_undefined()
        && !budget
            .as_number()
            .is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n >= 0.0)
    {
        return Err(RouteError::new(
            "INVALID_THINKING_CONFIG",
            "budgetTokens must be a non-negative integer.",
        ));
    }
    Ok(())
}
