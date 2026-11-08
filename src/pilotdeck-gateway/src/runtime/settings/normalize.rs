//! Validate the public localSettings allowlist before touching the file.
use super::SettingsError;
use pilotdeck_protocol::{JsObject, JsString, JsValue};

#[derive(Debug)]
pub(super) struct Action {
    pub path: Vec<&'static str>,
    pub value: Option<JsValue>,
}
fn unsupported(path: &str) -> SettingsError {
    SettingsError::local_settings(
        "UNSUPPORTED_LOCAL_SETTING",
        format!("localSettings.{path} is not supported by PilotDeck localSettings."),
    )
}
fn invalid(path: &str, detail: &str) -> SettingsError {
    SettingsError::local_settings(
        "INVALID_LOCAL_SETTINGS",
        format!("localSettings.{path} {detail}"),
    )
}
fn object<'a>(v: &'a JsValue, path: &str, detail: &str) -> Result<&'a JsObject, SettingsError> {
    v.as_object().ok_or_else(|| invalid(path, detail))
}
fn keys(value: &JsObject, path: &str, allowed: &[&str]) -> Result<(), SettingsError> {
    for (key, _) in value.entries() {
        if !allowed.iter().any(|allowed| key.equals_str(allowed)) {
            let key = String::from_utf16_lossy(key.units());
            let full = if path.is_empty() {
                key
            } else {
                format!("{path}.{key}")
            };
            return Err(unsupported(&full));
        }
    }
    Ok(())
}
fn integer(v: &JsValue, minimum: f64, safe: bool) -> bool {
    v.as_number().is_some_and(|n| {
        n.is_finite() && n.fract() == 0.0 && n >= minimum && (!safe || n <= 9_007_199_254_740_991.0)
    })
}
fn action(out: &mut Vec<Action>, path: &[&'static str], value: &JsValue) {
    if !value.is_undefined() {
        out.push(Action {
            path: path.to_vec(),
            value: if matches!(value, JsValue::Null) {
                None
            } else {
                Some(value.clone())
            },
        });
    }
}
fn field<'a>(v: &'a JsValue, name: &str) -> &'a JsValue {
    static U: JsValue = JsValue::Undefined;
    v.get(name).unwrap_or(&U)
}
fn nullable_boolean(v: &JsValue, path: &str) -> Result<(), SettingsError> {
    if matches!(v, JsValue::Undefined | JsValue::Null | JsValue::Bool(_)) {
        Ok(())
    } else {
        Err(invalid(path, "must be a boolean or null."))
    }
}
/// Actions are emitted in the TS editor's fixed path order, not input key order.
pub(super) fn normalize(settings: &JsValue) -> Result<Vec<Action>, SettingsError> {
    let root = object(settings, "", "must be an object.")?;
    keys(root, "", &["agent", "extension", "tools"])?;
    let mut out = Vec::new();
    let agent = field(settings, "agent");
    if !agent.is_undefined() {
        let obj = object(agent, "agent", "must be an object.")?;
        keys(
            obj,
            "agent",
            &[
                "maxContextTokens",
                "maxOutputTokens",
                "thinking",
                "subagents",
            ],
        )?;
        for (name, path) in [
            ("maxContextTokens", vec!["agent", "maxContextTokens"]),
            ("maxOutputTokens", vec!["agent", "maxOutputTokens"]),
        ] {
            let v = field(agent, name);
            if !matches!(v, JsValue::Undefined | JsValue::Null) && !integer(v, 1.0, false) {
                return Err(invalid(
                    &format!("agent.{name}"),
                    "must be a positive integer or null.",
                ));
            }
            action(&mut out, &path, v);
        }
        let thinking = field(agent, "thinking");
        if !matches!(thinking, JsValue::Undefined | JsValue::Null) {
            let obj = object(thinking, "agent.thinking", "must be an object or null.")?;
            keys(obj, "agent.thinking", &["enabled", "budgetTokens"])?;
            if !matches!(field(thinking, "enabled"), JsValue::Bool(_)) {
                return Err(invalid("agent.thinking.enabled", "must be a boolean."));
            }
            let n = field(thinking, "budgetTokens");
            if !n.is_undefined() && !integer(n, 0.0, false) {
                return Err(invalid(
                    "agent.thinking.budgetTokens",
                    "must be a non-negative integer.",
                ));
            }
        }
        if let JsValue::Object(_) = thinking {
            let mut normalized = JsObject::new();
            normalized.insert("enabled".into(), field(thinking, "enabled").clone());
            if !field(thinking, "budgetTokens").is_undefined() {
                normalized.insert(
                    "budgetTokens".into(),
                    field(thinking, "budgetTokens").clone(),
                );
            }
            action(
                &mut out,
                &["agent", "thinking"],
                &JsValue::Object(normalized),
            );
        } else {
            action(&mut out, &["agent", "thinking"], thinking);
        }
        let sub = field(agent, "subagents");
        if matches!(sub, JsValue::Null) {
            action(&mut out, &["agent", "subagents"], sub);
        } else if !sub.is_undefined() {
            let obj = object(sub, "agent.subagents", "must be an object or null.")?;
            keys(
                obj,
                "agent.subagents",
                &["default", "timeoutMs", "maxDepth"],
            )?;
            let model = field(sub, "default");
            if !matches!(model, JsValue::Undefined | JsValue::Null) {
                let name = model.as_string().ok_or_else(|| {
                    invalid(
                        "agent.subagents.default",
                        "must be a non-empty \"provider/model\" string, \"inherit\", or null.",
                    )
                })?;
                let trimmed = crate::runtime::memory::js_trim_units(name.units());
                if trimmed.is_empty() {
                    return Err(invalid(
                        "agent.subagents.default",
                        "must be a non-empty \"provider/model\" string, \"inherit\", or null.",
                    ));
                }
                action(
                    &mut out,
                    &["agent", "subagents", "default"],
                    &JsValue::String(JsString::from_utf16(trimmed.to_vec())),
                );
            } else {
                action(&mut out, &["agent", "subagents", "default"], model);
            }
            for (name, min, safe, detail) in [
                (
                    "timeoutMs",
                    1.0,
                    false,
                    "must be a positive integer or null.",
                ),
                (
                    "maxDepth",
                    0.0,
                    true,
                    "must be a non-negative safe integer or null.",
                ),
            ] {
                let value = field(sub, name);
                if !matches!(value, JsValue::Undefined | JsValue::Null)
                    && !integer(value, min, safe)
                {
                    return Err(invalid(&format!("agent.subagents.{name}"), detail));
                }
                let path = if name == "timeoutMs" {
                    vec!["agent", "subagents", "timeoutMs"]
                } else {
                    vec!["agent", "subagents", "maxDepth"]
                };
                action(&mut out, &path, value);
            }
        }
    }
    let extension = field(settings, "extension");
    if !extension.is_undefined() {
        let obj = object(extension, "extension", "must be an object.")?;
        keys(
            obj,
            "extension",
            &["includeHookEvents", "builtinPluginsEnabled"],
        )?;
        let value = field(extension, "includeHookEvents");
        nullable_boolean(value, "extension.includeHookEvents")?;
        action(&mut out, &["extension", "includeHookEvents"], value);
        let plugins = field(extension, "builtinPluginsEnabled");
        if !matches!(plugins, JsValue::Undefined | JsValue::Null) {
            let obj = object(
                plugins,
                "extension.builtinPluginsEnabled",
                "must be an object of booleans or null.",
            )?;
            for (name, v) in obj.entries() {
                if crate::runtime::memory::js_trim_units(name.units()).is_empty()
                    || !matches!(v, JsValue::Bool(_))
                {
                    return Err(invalid(
                        "extension.builtinPluginsEnabled",
                        "must be an object with non-empty keys and boolean values.",
                    ));
                }
            }
        }
        if let JsValue::Object(obj) = plugins {
            let mut normalized = JsObject::new();
            for (name, value) in obj.entries() {
                // Assigning a primitive to __proto__ on a normal JS object
                // invokes its inherited setter and creates no own property.
                if !name.equals_str("__proto__") {
                    normalized.insert(name.clone(), value.clone());
                }
            }
            action(
                &mut out,
                &["extension", "builtinPluginsEnabled"],
                &JsValue::Object(normalized),
            );
        } else {
            action(&mut out, &["extension", "builtinPluginsEnabled"], plugins);
        }
    }
    let tools = field(settings, "tools");
    if !tools.is_undefined() {
        let obj = object(tools, "tools", "must be an object.")?;
        keys(obj, "tools", &["webSearch"])?;
        let web = field(tools, "webSearch");
        if !web.is_undefined() {
            let obj = object(web, "tools.webSearch", "must be an object.")?;
            keys(obj, "tools.webSearch", &["enabled"])?;
            let enabled = field(web, "enabled");
            nullable_boolean(enabled, "tools.webSearch.enabled")?;
            action(&mut out, &["tools", "webSearch", "enabled"], enabled);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    #[test]
    fn allowed_settings_actions_match_ts_writer() {
        let oracle = parse_json(include_str!(
            "../../../tests/fixtures/native-settings/writer-oracle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = field(&oracle, "cases") else {
            panic!("cases")
        };
        for case in cases {
            let id = field(case, "id").as_string().unwrap().to_utf8().unwrap();
            let result = normalize(field(case, "settings"));
            if let Some(expected) = case.get("error") {
                let error = result.unwrap_err();
                assert_eq!(error.code(), field(expected, "code").as_string(), "{id}");
                assert_eq!(
                    error.message(),
                    field(expected, "message").as_string().unwrap(),
                    "{id}"
                );
            } else {
                let actions = result.unwrap();
                let actual_applied: Vec<_> = actions
                    .iter()
                    .filter(|a| a.value.is_some())
                    .map(|a| a.path.join("."))
                    .collect();
                let actual_cleared: Vec<_> = actions
                    .iter()
                    .filter(|a| a.value.is_none())
                    .map(|a| a.path.join("."))
                    .collect();
                let expected = field(case, "result");
                for (actual, key) in [(&actual_applied, "applied"), (&actual_cleared, "cleared")] {
                    let JsValue::Array(values) = field(expected, key) else {
                        panic!("{id} {key}")
                    };
                    let values: Vec<_> = values
                        .iter()
                        .map(|v| v.as_string().unwrap().to_utf8().unwrap())
                        .collect();
                    assert_eq!(actual, &values, "{id} {key}");
                }
            }
        }
    }
}
