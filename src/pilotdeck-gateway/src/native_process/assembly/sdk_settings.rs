//! Native SDK setting-source IO policy; parsing and precedence remain in the
//! existing SDK session owner. The same descriptor is checked and consumed.
use crate::{
    config::{self, BoundedSourceReadError},
    runtime::sdk_session::{
        SdkSessionError, SdkSettingSourceReadError, SdkSettingSourceReader, SettingSource,
    },
};
use std::path::Path;
pub(super) struct NativeSettingSourceReader;
impl SdkSettingSourceReader for NativeSettingSourceReader {
    fn read(
        &self,
        source: SettingSource,
        path: &Path,
    ) -> Result<Vec<u8>, SdkSettingSourceReadError> {
        config::read_setting_source_bounded(path).map_err(|error| match error {
            BoundedSourceReadError::Io(error) => SdkSettingSourceReadError::Io(error),
            BoundedSourceReadError::Resource(error) => {
                SdkSettingSourceReadError::Refused(SdkSessionError::new(
                    "SDK_SETTING_SOURCE_RESOURCE_LIMIT",
                    format!(
                        "Unable to read {} settings source: {}",
                        source.as_str(),
                        String::from_utf16_lossy(error.message.units())
                    ),
                ))
            }
        })
    }
}

fn field<'a>(value: &'a pilotdeck_protocol::JsValue, key: &str) -> &'a pilotdeck_protocol::JsValue {
    static U: pilotdeck_protocol::JsValue = pilotdeck_protocol::JsValue::Undefined;
    value.get(key).unwrap_or(&U)
}
/// The settings resolver has already applied source precedence and enforced
/// host overlays. Null model retains the project model; null child default clears it.
pub(super) fn compose_agent(
    agent: &pilotdeck_protocol::JsValue,
    sdk: Option<&pilotdeck_protocol::JsValue>,
    _snapshot: &pilotdeck_protocol::JsValue,
) -> pilotdeck_protocol::JsValue {
    use pilotdeck_protocol::JsValue;

    let Some(sdk) = sdk else {
        return agent.clone();
    };
    let overlay = field(field(sdk, "settings"), "agent");
    let mut result = agent.as_object().cloned().unwrap_or_default();
    if let Some(requested) = field(overlay, "model").as_string() {
        result.insert(
            "model".into(),
            resolve_runtime_model(requested, field(agent, "model")),
        );
    }
    for name in ["maxContextTokens", "maxOutputTokens", "thinking"] {
        if let Some(value) = overlay
            .get(name)
            .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
        {
            result.insert(name.into(), value.clone());
        }
    }
    if let Some(settings) = field(overlay, "subagents").as_object() {
        let mut subagents = field(agent, "subagents")
            .as_object()
            .cloned()
            .unwrap_or_default();
        for key in ["timeoutMs", "maxDepth"] {
            if let Some(v) = settings.get(key) {
                subagents.insert(key.into(), v.clone());
            }
        }
        if let Some(default) = settings.get("default") {
            let selected = match default {
                JsValue::Null => JsValue::Null,
                JsValue::String(value) if value.equals_str("inherit") => {
                    field(&JsValue::Object(result.clone()), "model").clone()
                }
                JsValue::String(value) => {
                    resolve_runtime_model(value, field(&JsValue::Object(result.clone()), "model"))
                }
                _ => JsValue::Undefined,
            };
            subagents.insert("default".into(), selected);
        }
        result.insert("subagents".into(), JsValue::Object(subagents));
    }
    JsValue::Object(result)
}

/// SessionAgentConfigBundle.resolveSdkModel is deliberately distinct from the
/// registry's catalog validation: short ids retain the parent at runtime.
fn resolve_runtime_model(
    value: &pilotdeck_protocol::JsString,
    fallback: &pilotdeck_protocol::JsValue,
) -> pilotdeck_protocol::JsValue {
    use pilotdeck_protocol::{JsString, JsValue};
    let units = value.units();
    match units.iter().position(|u| *u == u16::from(b'/')) {
        Some(i) if i > 0 && i + 1 < units.len() => crate::metadata::object([
            (
                "provider",
                JsValue::String(JsString::from_utf16(units[..i].to_vec())),
            ),
            (
                "model",
                JsValue::String(JsString::from_utf16(units[i + 1..].to_vec())),
            ),
        ]),
        _ => fallback.clone(),
    }
}

pub(super) fn source_environment(env: &crate::config::Env, home: &Path) -> crate::config::Env {
    let mut env = env.clone();
    env.insert("PILOT_HOME".into(), home.to_string_lossy().into_owned());
    env
}

#[cfg(test)]
#[path = "sdk_settings_tests.rs"]
mod tests;
