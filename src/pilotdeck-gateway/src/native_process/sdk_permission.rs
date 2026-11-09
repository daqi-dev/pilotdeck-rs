//! SDK permission composition; the registry still owns grants and lease lifetime.
use crate::{config::permission::RuleSetHandle, runtime::turn::submit::SubmitError};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
fn strings(value: Option<&JsValue>) -> Vec<JsString> {
    match value {
        Some(JsValue::Array(v)) => v.iter().filter_map(JsValue::as_string).cloned().collect(),
        _ => Vec::new(),
    }
}
/// Preserve SDK policy rule order/duplicates and the underlying mutable arrays.
pub(super) fn rules(config: &JsObject, base: &RuleSetHandle) -> RuleSetHandle {
    let policy = config.get("managedPermissions");
    let deny = strings(policy.and_then(|p| p.get("deny")))
        .iter()
        .map(|e| crate::runtime::interaction::policy_rule(e, false))
        .collect::<Vec<_>>();
    let ask = strings(policy.and_then(|p| p.get("ask")))
        .iter()
        .map(|e| crate::runtime::interaction::policy_rule(e, true))
        .collect::<Vec<_>>();
    base.with_policy_rules(&deny, &ask)
}
pub(super) fn directories(config: &JsObject) -> Vec<JsString> {
    let mut out = Vec::new();
    for dir in strings(config.get("additionalWorkingDirectories")) {
        if !out.contains(&dir) {
            out.push(dir);
        }
    }
    out
}
pub(super) fn project(target: &mut JsObject, sdk: &JsValue) {
    if let Some(v) = sdk.get("managedPermissions").filter(|v| !v.is_undefined()) {
        target.insert("managedPermissions".into(), v.clone());
    }
    if let Some(v) = sdk.get("permissionMode").filter(|v| !v.is_undefined()) {
        target.insert("nativeSdkPermissionMode".into(), v.clone());
    }
    if let Some(v) = sdk
        .get("additionalWorkingDirectories")
        .filter(|v| !v.is_undefined())
    {
        let mut dirs = directories(target);
        for dir in strings(Some(v)) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        target.insert(
            "additionalWorkingDirectories".into(),
            JsValue::Array(dirs.into_iter().map(JsValue::String).collect()),
        );
    }
}
pub(super) fn validate(sdk: &JsValue) -> Result<(), SubmitError> {
    if let Some(v) = sdk.get("managedPermissions").filter(|v| !v.is_undefined()) {
        let valid=v.as_object().is_some_and(|v|v.entries().iter().all(|(k,_)|["deny","ask","defaultMode","canPrompt"].iter().any(|n|k.equals_str(n)))
            &&["deny","ask"].iter().all(|k|matches!(v.get(k),Some(JsValue::Array(items)) if items.iter().all(|i|i.as_string().is_some_and(|s|!crate::runtime::memory::js_trim_units(s.units()).is_empty()))))
            &&v.get("defaultMode").is_none_or(|v|v.is_undefined()||v.as_string().is_some_and(|s|s.equals_str("plan")))
            &&v.get("canPrompt").is_none_or(|v|v.is_undefined()||v==&JsValue::Bool(false)));
        if !valid {
            return Err(SubmitError::new(
                "UNSUPPORTED_SDK_MANAGED_SETTING",
                "managedPermissions may contain deny/ask entries, defaultMode=plan, and canPrompt=false only.",
            ));
        }
    }
    if let Some(v) = sdk
        .get("additionalWorkingDirectories")
        .filter(|v| !v.is_undefined())
        && !matches!(v,JsValue::Array(items) if items.iter().all(|i|i.as_string().is_some_and(|s|s.units().first()==Some(&47))))
    {
        return Err(SubmitError::new(
            "INVALID_SDK_SESSION_CONFIG",
            "additionalWorkingDirectories must contain absolute paths.",
        ));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::permission::PermissionRule,
        metadata::object,
        runtime::permission::{self, DefaultPermissionContextOptions},
    };
    #[test]
    fn actual_ts_sdk_permission_composition_utf16_rules_modes_and_roots() {
        let fixture = pilotdeck_protocol::parse_json(include_str!(
            "../../tests/fixtures/sdk-permissions/oracle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = fixture.get("cases").unwrap() else {
            panic!("cases")
        };
        let mut count = 0;
        for case in cases {
            if case.get("organizationPolicy").is_some() {
                continue;
            } // organization policy belongs to its existing owner.
            let sdk = case.get("sdk").unwrap();
            validate(sdk).unwrap();
            let mut config = object([(
                "additionalWorkingDirectories",
                JsValue::Array(vec!["/base".into()]),
            )])
            .as_object()
            .unwrap()
            .clone();
            project(&mut config, sdk);
            let base = RuleSetHandle::new(crate::config::permission::PermissionRuleSet {
                allow: vec![PermissionRule::new("session", "allow", "write_file")],
                deny: vec![PermissionRule::new("project", "deny", "base_denied")],
                ask: vec![PermissionRule::new("session", "ask", "base_asked")],
            });
            let policy = rules(&config, &base);
            let stored = case
                .get("stored")
                .and_then(JsValue::as_string)
                .cloned()
                .or(Some("bypassPermissions".into()));
            let mode = permission::effective_permission_mode(&config, stored);
            let disabled = config
                .get("managedPermissions")
                .and_then(|v| v.get("canPrompt"))
                == Some(&JsValue::Bool(false));
            let sdk_mode = sdk.get("permissionMode").and_then(JsValue::as_string);
            let context =
                permission::create_default_permission_context(DefaultPermissionContextOptions {
                    cwd: "/project".into(),
                    mode: Some(mode.clone().into()),
                    rules: policy.snapshot(),
                    can_prompt: Some(
                        !disabled && !sdk_mode.is_some_and(|s| s.equals_str("dontAsk")),
                    ),
                    policy_can_prompt: disabled.then_some(false),
                    bypass_available: Some(true),
                    accept_edits: Some(sdk_mode.is_some_and(|s| s.equals_str("acceptEdits"))),
                    additional_working_directories: Some(directories(&config)),
                    ..Default::default()
                });
            assert_eq!(
                JsValue::String(mode),
                case.get("permissionMode").unwrap().clone(),
                "{:?}",
                case.get("name")
            );
            assert_eq!(
                context.to_value(),
                case.get("context").unwrap().clone(),
                "{:?}",
                case.get("name")
            );
            count += 1;
        }
        assert_eq!(count, 11);
    }
    #[test]
    fn sdk_policy_keeps_live_grants_and_mcp_ask_and_rejects_broadening() {
        let base = RuleSetHandle::empty();
        let config = object([(
            "managedPermissions",
            object([
                ("deny", JsValue::Array(vec!["Write".into()])),
                ("ask", JsValue::Array(vec!["Read".into(), "Read".into()])),
            ]),
        )])
        .as_object()
        .unwrap()
        .clone();
        let overlay = rules(&config, &base);
        base.update_member("allow", |v| {
            v.push(PermissionRule::new("session", "allow", "write_file").into())
        });
        base.update_member("ask", |v| {
            v.push(PermissionRule::new("session", "ask", "mcp__one").into())
        });
        assert_eq!(overlay.snapshot().allow.len(), 1);
        assert_eq!(overlay.snapshot().ask.len(), 3);
        overlay.update_member("ask", Vec::clear);
        assert_eq!(overlay.snapshot().ask.len(), 2);
        overlay.update_member("deny", Vec::clear);
        assert_eq!(overlay.snapshot().deny.len(), 1);
        assert!(base.snapshot().deny.is_empty());
        for sdk in [
            object([("managedPermissions", JsValue::Null)]),
            object([(
                "managedPermissions",
                object([
                    ("deny", JsValue::Array(vec![])),
                    ("ask", JsValue::Array(vec![])),
                    ("canPrompt", JsValue::Bool(true)),
                ]),
            )]),
            object([(
                "additionalWorkingDirectories",
                JsValue::Array(vec!["relative".into()]),
            )]),
        ] {
            assert!(validate(&sdk).is_err());
        }
    }
}
