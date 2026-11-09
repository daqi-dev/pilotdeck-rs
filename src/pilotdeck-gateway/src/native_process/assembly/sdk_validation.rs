//! Legacy Gateway SDK intake order. Unknown SDK keys are not capability claims.
//! Runtime owners still validate catalogs, policies and native resource limits.
use crate::runtime::turn::submit::SubmitError;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
fn get<'a>(v: &'a JsValue, k: &str) -> Option<&'a JsValue> {
    v.get(k).filter(|v| !v.is_undefined())
}
fn err(message: impl Into<String>) -> SubmitError {
    SubmitError::new("INVALID_SDK_SESSION_CONFIG", message)
}
fn trim(s: &JsString) -> bool {
    !crate::runtime::memory::js_trim_units(s.units()).is_empty()
}
fn text(v: &JsValue) -> bool {
    v.as_string().is_some_and(trim)
}
fn unique_skills(value: &JsValue) -> bool {
    let JsValue::Array(values) = value else {
        return false;
    };
    let mut seen = std::collections::HashSet::new();
    !values.is_empty()
        && values.iter().all(|value| {
            value
                .as_string()
                .is_some_and(|name| trim(name) && seen.insert(name))
        })
}
fn distinct_plugin_paths(value: &JsValue) -> bool {
    let JsValue::Array(values) = value else {
        return false;
    };
    let mut seen = std::collections::HashSet::new();
    !values.is_empty()
        && values.iter().all(|plugin| {
            plugin.get("type") == Some(&JsValue::from("local"))
                && plugin
                    .get("path")
                    .and_then(JsValue::as_string)
                    .is_some_and(|path| path.units().first() == Some(&47) && seen.insert(path))
        })
}
fn keys(v: &JsObject, allowed: &[&str]) -> bool {
    v.own_entries()
        .iter()
        .all(|(k, _)| allowed.iter().any(|a| k.equals_str(a)))
}
fn integer(v: &JsValue, min: f64, safe: bool) -> bool {
    v.as_number().is_some_and(|n| {
        n.is_finite() && n.fract() == 0.0 && n >= min && (!safe || n <= 9_007_199_254_740_991.0)
    })
}
fn list(v: &JsValue, allowed: &[&str]) -> bool {
    matches!(v,JsValue::Array(a) if !a.is_empty()&&a.iter().enumerate().all(|(i,v)|v.as_string().is_some_and(|s|allowed.iter().any(|a|s.equals_str(a)))&&!a[..i].contains(v)))
}
fn single(k: &str, v: &JsValue) -> JsValue {
    let mut o = JsObject::new();
    o.insert(k.into(), v.clone());
    JsValue::Object(o)
}
fn check_string(config: &JsValue, key: &str) -> Result<(), SubmitError> {
    if get(config, key).is_some_and(|v| v.as_string().is_none()) {
        return Err(err(format!("{key} must be a string.")));
    }
    Ok(())
}
fn settings(value: &JsValue) -> Result<(), SubmitError> {
    let fail = |s: &str| SubmitError::new("INVALID_SDK_SETTINGS", s);
    let o = value
        .as_object()
        .ok_or_else(|| fail("settings must be an object."))?;
    if !keys(o, &["agent"]) {
        return Err(SubmitError::new(
            "UNSUPPORTED_SDK_SETTING",
            "Only settings.agent is supported by the SDK session overlay.",
        ));
    }
    let Some(agent) = get(value, "agent") else {
        return Ok(());
    };
    let a = agent
        .as_object()
        .filter(|a| {
            keys(
                a,
                &[
                    "model",
                    "fallbackModel",
                    "maxContextTokens",
                    "maxOutputTokens",
                    "thinking",
                    "subagents",
                ],
            )
        })
        .ok_or_else(|| fail("settings.agent contains unsupported fields."))?;
    for key in ["model", "fallbackModel"] {
        if a.get(key)
            .is_some_and(|v| !matches!(v, JsValue::Null | JsValue::Undefined) && !text(v))
        {
            return Err(fail(&format!(
                "settings.agent.{key} must be a non-empty model id or null."
            )));
        }
    }
    for key in ["maxContextTokens", "maxOutputTokens"] {
        if get(agent, key).is_some_and(|v| !integer(v, 1.0, false)) {
            return Err(fail(&format!(
                "settings.agent.{key} must be a positive integer."
            )));
        }
    }
    if let Some(t) = get(agent, "thinking")
        && !t.as_object().is_some_and(|o| {
            keys(o, &["enabled", "budgetTokens"])
                && o.get("enabled").is_some_and(|v| v.as_bool().is_some())
                && get(t, "budgetTokens").is_none_or(|v| integer(v, 0.0, false))
        })
    {
        return Err(fail(
            "settings.agent.thinking must contain enabled and an optional non-negative integer budgetTokens.",
        ));
    }
    if let Some(s) = get(agent, "subagents")
        && !s.as_object().is_some_and(|o| {
            keys(o, &["default", "timeoutMs", "maxDepth"])
                && get(s, "default").is_none_or(|v| matches!(v, JsValue::Null) || text(v))
                && get(s, "timeoutMs").is_none_or(|v| integer(v, 1.0, false))
                && get(s, "maxDepth").is_none_or(|v| integer(v, 0.0, true))
        })
    {
        return Err(fail(
            "settings.agent.subagents supports a non-empty default model id, positive integer timeoutMs, and non-negative safe integer maxDepth.",
        ));
    }
    Ok(())
}
fn selector(value: &JsValue) -> bool {
    let Some(s) = value.as_string() else {
        return false;
    };
    if s.equals_str("*") {
        return true;
    }
    let pieces = s.units().split(|u| *u == 47).collect::<Vec<_>>();
    pieces.len() == 2
        && !pieces[0].is_empty()
        && !pieces[1].is_empty()
        && pieces[0].iter().all(|u| *u != 42 && !space(*u))
        && (pieces[1] == [42] || pieces[1].iter().all(|u| *u != 42 && !space(*u)))
}
fn space(u: u16) -> bool {
    crate::runtime::memory::js_trim_units(&[u]).is_empty()
}
fn agent_trim(v: Option<&JsValue>, label: &str) -> Result<bool, SubmitError> {
    match v {
        None | Some(JsValue::Null | JsValue::Undefined) => Ok(false),
        Some(JsValue::String(s)) => Ok(trim(s)),
        _ => Err(SubmitError::new(
            "gateway_request_failed",
            format!("agent.{label}?.trim is not a function"),
        )),
    }
}
fn tools_valid(value: &JsValue) -> Result<bool, SubmitError> {
    let JsValue::Array(a) = value else {
        return Ok(false);
    };
    for v in a {
        match v {
            JsValue::String(s) => {
                if !trim(s) {
                    return Ok(false);
                }
            }
            JsValue::Null => {
                return Err(SubmitError::new(
                    "gateway_request_failed",
                    "Cannot read properties of null (reading 'trim')",
                ));
            }
            JsValue::Undefined => {
                return Err(SubmitError::new(
                    "gateway_request_failed",
                    "Cannot read properties of undefined (reading 'trim')",
                ));
            }
            _ => {
                return Err(SubmitError::new(
                    "gateway_request_failed",
                    "tool.trim is not a function",
                ));
            }
        }
    }
    Ok(true)
}
fn agents(value: &JsValue) -> Result<(), SubmitError> {
    let o = value
        .as_object()
        .ok_or_else(|| err("agents must be an object."))?;
    for (name, a) in o.own_entries() {
        let bad = || {
            err(format!(
                "Agent {} is invalid.",
                if name.is_empty() {
                    "<unnamed>".into()
                } else {
                    String::from_utf16_lossy(name.units())
                }
            ))
        };
        if !trim(name) || a.as_object().is_none() {
            return Err(bad());
        }
        if !agent_trim(a.get("description"), "description")?
            || !agent_trim(a.get("prompt"), "prompt")?
        {
            return Err(bad());
        }
        if get(a, "model").is_some_and(|v| !text(v)) {
            return Err(bad());
        }
        for key in ["tools", "disallowedTools"] {
            if let Some(v) = get(a, key)
                && !tools_valid(v)?
            {
                return Err(bad());
            }
        }
        if get(a, "maxTurns").is_some_and(|v| !integer(v, 1.0, false))
            || get(a, "background").is_some_and(|v| v.as_bool().is_none())
            || get(a, "observer").is_some_and(|v| !text(v))
            || get(a, "observerMessage").is_some_and(|v| v.as_string().is_none())
        {
            return Err(bad());
        }
        if a.get("observerMessage")
            .and_then(JsValue::as_string)
            .is_some_and(trim)
            && get(a, "observer").is_none()
        {
            return Err(bad());
        }
        for (key, values) in [
            ("effort", &["low", "medium", "high"][..]),
            (
                "permissionMode",
                &["default", "plan", "bypassPermissions"][..],
            ),
        ] {
            if get(a, key).is_some_and(|v| {
                !v.as_string()
                    .is_some_and(|s| values.iter().any(|v| s.equals_str(v)))
            }) {
                return Err(bad());
            }
        }
    }
    for (name, a) in o.own_entries() {
        if let Some(observer) = get(a, "observer").and_then(JsValue::as_string)
            && (observer == name || o.get_key(observer).is_none())
        {
            return Err(err(format!(
                "Agent {} observer must reference a distinct configured AgentDefinition.",
                String::from_utf16_lossy(name.units())
            )));
        }
    }
    Ok(())
}
pub(super) fn validate(config: &JsValue) -> Result<(), SubmitError> {
    if !matches!(config, JsValue::Object(_) | JsValue::Array(_)) {
        return Err(err("sdkSessionConfig must be an object."));
    }
    // JSON arrays have no named SDK properties; do not erase their indexed data.
    if matches!(config, JsValue::Array(_)) {
        return Ok(());
    }
    check_string(config, "systemPrompt")?;
    if get(config, "persistSession").is_some_and(|v| v != &JsValue::Bool(false)) {
        return Err(err("persistSession must be false when specified."));
    }
    check_string(config, "appendSystemPrompt")?;
    check_string(config, "planModeInstructions")?;
    if get(config, "outputStyle").is_some_and(|v| !text(v)) {
        return Err(err("outputStyle must be a non-empty string."));
    }
    if get(config, "permissionMode").is_some_and(|v| {
        !v.as_string()
            .is_some_and(|s| s.equals_str("acceptEdits") || s.equals_str("dontAsk"))
    }) {
        return Err(err("permissionMode must be acceptEdits or dontAsk."));
    }
    if let Some(v) = get(config, "managedPermissions") {
        crate::native_process::sdk_permission::validate(&single("managedPermissions", v))?;
    }
    if let Some(v) = get(config, "managedModels")
        && !v.as_object().is_some_and(|_| {
            ["allow", "deny"]
                .iter()
                .all(|k| matches!(v.get(k),Some(JsValue::Array(a))if a.iter().all(selector)))
        })
    {
        return Err(SubmitError::new(
            "INVALID_SDK_MANAGED_SETTINGS",
            "managedModels selectors must be *, provider/*, or provider/model.",
        ));
    }
    for key in [
        "includeHookEvents",
        "agentProgressSummaries",
        "forwardSubagentText",
    ] {
        if get(config, key).is_some_and(|v| v.as_bool().is_none()) {
            return Err(err(format!("{key} must be a boolean.")));
        }
    }
    crate::native_process::sdk_tool_policy::validate_sandbox(config)?;
    if get(config, "userDialogKinds")
        .is_some_and(|v| !list(v, &["input", "select", "confirm", "form"]))
    {
        return Err(err(
            "userDialogKinds must be a non-empty unique array containing only input, select, confirm, or form.",
        ));
    }
    super::super::task_budget::validate(config)?;
    if let Some(s) = get(config, "settings") {
        settings(s)?;
    }
    if get(config, "settingSources")
        .is_some_and(|v| !list(v, &["managed", "user", "project", "local"]))
    {
        return Err(SubmitError::new(
            "INVALID_SDK_SETTING_SOURCES",
            "settingSources must be a non-empty unique array containing only managed, user, project, or local.",
        ));
    }
    if let Some(v) = get(config, "additionalWorkingDirectories") {
        crate::native_process::sdk_permission::validate(&single(
            "additionalWorkingDirectories",
            v,
        ))?;
    }
    if let Some(v) = get(config, "toolAliases") {
        if matches!(v, JsValue::Null) {
            return Err(SubmitError::new(
                "gateway_request_failed",
                "Cannot convert undefined or null to object",
            ));
        }
        crate::native_process::sdk_tools::validate(&single("toolAliases", v))?;
    }
    if let Some(v) = get(config, "outputFormat") {
        if matches!(v, JsValue::Null) {
            return Err(SubmitError::new(
                "gateway_request_failed",
                "Cannot read properties of null (reading 'type')",
            ));
        }
        crate::native_process::sdk_tools::validate(&single("outputFormat", v))?;
    }
    if let Some(v) = get(config, "agents") {
        agents(v)?;
    }
    if let Some(v) = get(config, "skills")
        && !v.as_string().is_some_and(|s| s.equals_str("all"))
        && !unique_skills(v)
    {
        return Err(err(
            "skills must be all or a non-empty array of unique skill names.",
        ));
    }
    if let Some(v) = get(config, "plugins")
        && !distinct_plugin_paths(v)
    {
        return Err(err(
            "plugins must be a non-empty array of distinct absolute Gateway-local plugin paths.",
        ));
    }
    // URL validation must precede Object.entries(null) on headers.
    if let Some(h) = get(config, "hooks")
        && h.get("headers") == Some(&JsValue::Null)
    {
        let mut no_headers = h.as_object().cloned().unwrap_or_default();
        no_headers.insert("headers".into(), JsValue::Undefined);
        no_headers.insert("events".into(), JsValue::Object(JsObject::new()));
        super::super::sdk_hooks::validate(&single("hooks", &JsValue::Object(no_headers)))?;
        return Err(SubmitError::new(
            "gateway_request_failed",
            "Cannot convert undefined or null to object",
        ));
    }
    super::super::sdk_hooks::validate(config)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fifty_seven_actual_ts_intake_cases_keep_order_and_unknown_fields() {
        let oracle = pilotdeck_protocol::parse_json(include_str!(
            "../../../tests/fixtures/sdk-intake/oracle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = oracle.get("cases").unwrap() else {
            panic!("cases")
        };
        assert_eq!(cases.len(), 57);
        for case in cases {
            let expected = case.get("expected").unwrap();
            let input = case.get("config").unwrap();
            let result = super::super::validate_supported_config(input);
            if expected.get("ok") == Some(&JsValue::Bool(true)) {
                assert!(
                    result.is_ok(),
                    "{:?}: {:?}",
                    case.get("name"),
                    result.err().map(|e| (e.code, e.message))
                );
            } else {
                let actual = result.unwrap_err();
                assert_eq!(
                    JsValue::String(actual.code.into()),
                    expected.get("code").unwrap().clone(),
                    "{:?}",
                    case.get("name")
                );
                assert_eq!(
                    JsValue::String(actual.message.into()),
                    expected.get("message").unwrap().clone(),
                    "{:?}",
                    case.get("name")
                );
            }
        }
    }
}
