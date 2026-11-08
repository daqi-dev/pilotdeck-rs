//! Explicit native invocation ports. Optional ports that are absent/null are
//! unconfigured; active unsupported ports fail before native input admission.
use crate::runtime::project::{RuntimeError, RuntimeJsError};
use serde_json::Value;
pub(crate) fn unavailable(port: &str) -> RuntimeError {
    let message = format!(
        "Kernel native port {port} is configured but this assembly does not install it; no input was accepted."
    );
    RuntimeError::new(message.clone()).with_js_error(RuntimeJsError {
        name: "KernelConfigurationError".into(),
        code: Some("KERNEL_UNCONFIGURED_PORT".into()),
        message: message.into(),
        requires_host: false,
    })
}
/// Product tool filters use the original PD catalog; the installed Kernel
/// catalog has no declared alias mapping. Never widen a configured restriction.
pub(crate) fn tool_filters(value: &Value) -> Result<(), RuntimeError> {
    if value
        .get("allowedTools")
        .is_some_and(|value| !value.is_null())
    {
        return Err(unavailable("allowedTools PD-to-Kernel catalog mapping"));
    }
    if value
        .get("disallowedTools")
        .is_some_and(|value| !value.is_null() && !value.as_array().is_some_and(Vec::is_empty))
    {
        return Err(unavailable("disallowedTools PD-to-Kernel catalog mapping"));
    }
    Ok(())
}
pub(crate) fn tool_filters_js(value: &pilotdeck_protocol::JsValue) -> Result<(), RuntimeError> {
    use pilotdeck_protocol::JsValue;
    if value
        .get("allowedTools")
        .is_some_and(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
    {
        return Err(unavailable("allowedTools PD-to-Kernel catalog mapping"));
    }
    if value.get("disallowedTools").is_some_and(|value| {
        !matches!(value, JsValue::Undefined | JsValue::Null)
            && !matches!(value,JsValue::Array(values) if values.is_empty())
    }) {
        return Err(unavailable("disallowedTools PD-to-Kernel catalog mapping"));
    }
    Ok(())
}
pub(crate) fn session_tool_filters(
    allowed: Option<&[pilotdeck_protocol::JsString]>,
    denied: Option<&[pilotdeck_protocol::JsString]>,
) -> Result<(), RuntimeError> {
    if allowed.is_some() {
        return Err(unavailable("allowedTools PD-to-Kernel catalog mapping"));
    }
    if denied.is_some_and(|names| !names.is_empty()) {
        return Err(unavailable("disallowedTools PD-to-Kernel catalog mapping"));
    }
    Ok(())
}
pub(crate) fn preflight(value: &Value) -> Result<(), RuntimeError> {
    tool_filters(value)?;
    for key in [
        "instructionLayers",
        "memoryContext",
        "promptContributions",
        "extensions",
        "instructionDiscovery",
        "memoryResolver",
        "extensionResolver",
        "promptCacheCoordinator",
    ] {
        if value.get(key).is_some_and(|v| !v.is_null()) {
            return Err(unavailable(key));
        }
    }
    for key in ["supportsPromptCache", "useProviderCount"] {
        if value.get(key).is_some_and(|v| !v.is_null() && v != false) {
            return Err(unavailable(key));
        }
    }
    for key in ["modelPricing", "pricingMap"] {
        if value
            .get(key)
            .is_some_and(|v| !v.is_null() && !v.as_object().is_some_and(|o| o.is_empty()))
        {
            return Err(unavailable(key));
        }
    }
    for key in ["agent", "options", "contextOverride"] {
        if let Some(child) = value.get(key) {
            preflight(child)?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn kernel_optional_ports_keep_unconfigured_null_false_empty_pricing_and_reject_active_values() {
        for value in [
            json!({}),
            json!({"allowedTools":null,"disallowedTools":[]}),
            json!({"memoryContext":null,"supportsPromptCache":false,"modelPricing":{}}),
            json!({"agent":{"extensions":null},"options":{"useProviderCount":null}}),
        ] {
            assert!(preflight(&value).is_ok());
        }
        for value in [
            json!({"allowedTools":[]}),
            json!({"allowedTools":["web.fetch"]}),
            json!({"disallowedTools":["web_fetch"]}),
            json!({"agent":{"disallowedTools":{}}}),
            json!({"memoryContext":[]}),
            json!({"agent":{"extensions":{}}}),
            json!({"options":{"supportsPromptCache":true}}),
            json!({"useProviderCount":"false"}),
            json!({"modelPricing":{"x":{"input":1}}}),
        ] {
            assert!(preflight(&value).is_err(), "{value}");
        }
    }
}
