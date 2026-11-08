//! Restrictive SDK catalog policy, matching ProjectSessionRuntimeBundle.ts
//! blockedToolNames/matchesToolSelector. This is distinct from the OS sandbox.
use crate::{
    host::HostError,
    runtime::tools::{ToolRegistry, registry::ToolPolicy},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

#[derive(Clone)]
struct Policy {
    managed: Option<(Vec<JsString>, Vec<JsString>)>,
    sandbox: Option<JsObject>,
    allowed: Option<Vec<JsString>>,
    denied: Option<Vec<JsString>>,
}
fn strings(value: Option<&JsValue>) -> Option<Vec<JsString>> {
    let Some(JsValue::Array(values)) = value else {
        return None;
    };
    Some(
        values
            .iter()
            .filter_map(JsValue::as_string)
            .cloned()
            .collect(),
    )
}
fn matches(name: &JsString, selector: &JsString) -> bool {
    selector.equals_str("*")
        || name == selector
        || selector
            .units()
            .strip_suffix(&[b'*' as u16])
            .is_some_and(|prefix| name.units().starts_with(prefix))
}
fn has(value: Option<&JsValue>, text: &str) -> bool {
    value
        .and_then(JsValue::as_string)
        .is_some_and(|value| value.equals_str(text))
}
impl Policy {
    fn allows(&self, fields: &JsObject) -> bool {
        let Some(name) = fields.get("name").and_then(JsValue::as_string) else {
            return false;
        };
        if self
            .allowed
            .as_ref()
            .is_some_and(|names| !names.contains(name))
            || self
                .denied
                .as_ref()
                .is_some_and(|names| names.contains(name))
        {
            return false;
        }
        if let Some((allowed, denied)) = &self.managed {
            if denied.iter().any(|selector| matches(name, selector)) {
                return false;
            }
            if !allowed.is_empty() && !allowed.iter().any(|selector| matches(name, selector)) {
                return false;
            }
        }
        let Some(sandbox) = &self.sandbox else {
            return true;
        };
        let kind = fields.get("kind");
        let bridge = ["shell", "mcp", "custom"]
            .iter()
            .any(|value| has(kind, value))
            || name.equals_str("execute_code")
            || name.equals_str("agent")
            || name.units().starts_with(&[116, 97, 115, 107, 95]);
        let process = has(sandbox.get("process"), "deny") && bridge;
        let filesystem = if has(sandbox.get("filesystem"), "deny") {
            has(kind, "filesystem") || bridge
        } else {
            has(sandbox.get("filesystem"), "read_only")
                && (bridge
                    || ["write_file", "edit_file", "edit_notebook"]
                        .iter()
                        .any(|value| name.equals_str(value)))
        };
        let network = has(sandbox.get("network"), "deny") && (has(kind, "network") || bridge);
        !process && !filesystem && !network
    }
}
/// Called after all ordinary families are registered, before ForkTurn captures
/// its registry and before ToolRuntime/permission share the final catalog.
/// A scoped view also applies to future register/refresh and aliases.
pub(super) fn scope(
    registry: ToolRegistry,
    config: &JsObject,
    allowed: Option<Vec<JsString>>,
    denied: Option<Vec<JsString>>,
) -> Result<ToolRegistry, HostError> {
    let managed = config
        .get("managedTools")
        .filter(|value| crate::runtime::turn::submit::truthy(value))
        .map(|value| {
            (
                strings(value.get("allow")).unwrap_or_default(),
                strings(value.get("deny")).unwrap_or_default(),
            )
        });
    if let Some(value) = config.get("managedTools") {
        crate::runtime::sdk_session::validate_selector_policy(
            Some(value),
            crate::runtime::sdk_session::SelectorKind::Tools,
        )
        .map_err(|error| crate::host::ModuleError {
            message: error.message.as_str().into(),
            code: Some(error.code.as_str().into()),
            canonical: None,
            retryable: None,
            retry_after_ms: None,
        })?;
    }
    if let Some(value) = config.get("sandbox") {
        validate_sandbox(&crate::metadata::object([("sandbox", value.clone())])).map_err(
            |error| crate::host::ModuleError {
                message: error.message.as_str().into(),
                code: Some(error.code.as_str().into()),
                canonical: None,
                retryable: None,
                retry_after_ms: None,
            },
        )?;
    }
    let sandbox = config.get("sandbox").and_then(JsValue::as_object).cloned();
    if managed.is_none() && sandbox.is_none() {
        return Ok(registry);
    }
    let policy = Policy {
        managed,
        sandbox,
        allowed,
        denied,
    };
    let predicate: ToolPolicy = Arc::new(move |tool| policy.allows(tool.fields()));
    registry.scoped(predicate)
}
/// Gateway preflight validates the SDK tool_policy vocabulary. Selector policy
/// validation stays in SdkSessionRegistry with its original admission ordering.
pub(super) fn validate_sandbox(
    config: &JsValue,
) -> Result<(), crate::runtime::turn::submit::SubmitError> {
    let Some(value) = config.get("sandbox").filter(|value| !value.is_undefined()) else {
        return Ok(());
    };
    let valid = value.as_object().is_some_and(|sandbox| {
        sandbox.entries().iter().all(|(key, _)| {
            ["type", "filesystem", "network", "process"]
                .iter()
                .any(|allowed| key.equals_str(allowed))
        }) && sandbox
            .get("type")
            .is_none_or(|value| value.is_undefined() || has(Some(value), "tool_policy"))
            && sandbox.get("filesystem").is_none_or(|value| {
                value.is_undefined() || has(Some(value), "read_only") || has(Some(value), "deny")
            })
            && ["network", "process"].iter().all(|key| {
                sandbox
                    .get(key)
                    .is_none_or(|value| value.is_undefined() || has(Some(value), "deny"))
            })
            && ["filesystem", "network", "process"]
                .iter()
                .any(|key| sandbox.get(key).is_some_and(|value| !value.is_undefined()))
    });
    if valid {
        Ok(())
    } else {
        Err(crate::runtime::turn::submit::SubmitError::new(
            "INVALID_SDK_SESSION_CONFIG",
            "sandbox must contain supported tool_policy restrictions.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules::capability::ToolDefinition;
    use crate::runtime::tools::{ExecutableTool, ToolFuture, ToolOutput, ToolRunContext};
    struct Tool(JsObject);
    impl ToolDefinition for Tool {
        fn fields(&self) -> &JsObject {
            &self.0
        }
    }
    impl ExecutableTool for Tool {
        fn execute(&self, _: JsValue, _: ToolRunContext) -> ToolFuture<ToolOutput> {
            Box::pin(async { panic!("blocked tool must never execute") })
        }
    }
    fn tool(name: &str, kind: &str) -> Arc<dyn ExecutableTool> {
        Arc::new(Tool(
            crate::metadata::object([("name", name.into()), ("kind", kind.into())])
                .as_object()
                .unwrap()
                .clone(),
        ))
    }
    #[test]
    fn scoped_catalog_filters_late_tools_and_aliases_by_canonical_name() {
        let registry = ToolRegistry::default();
        registry.register(tool("read_file", "filesystem")).unwrap();
        let policy = crate::metadata::object([(
            "managedTools",
            crate::metadata::object([
                ("allow", JsValue::Array(vec!["read*".into()])),
                ("deny", JsValue::Array(vec!["read_secret".into()])),
            ]),
        )]);
        let scoped = scope(registry.clone(), policy.as_object().unwrap(), None, None).unwrap();
        scoped.register(tool("read_late", "custom")).unwrap();
        scoped.register(tool("write_late", "custom")).unwrap();
        registry
            .register(tool("read_secret", "filesystem"))
            .unwrap();
        assert!(scoped.get(&"read_file".into()).is_some());
        assert!(scoped.get(&"read_late".into()).is_some());
        assert!(scoped.get(&"write_late".into()).is_none());
        assert!(scoped.get(&"read_secret".into()).is_none());
        let mut aliased = tool("write_alias_target", "filesystem").fields().clone();
        aliased.insert("aliases".into(), JsValue::Array(vec!["read_alias".into()]));
        scoped.register(Arc::new(Tool(aliased))).unwrap();
        assert!(
            scoped.get(&"read_alias".into()).is_none(),
            "alias cannot change the canonical policy target"
        );
        scoped.dispose();
        registry.dispose();
    }
    #[test]
    fn explicit_empty_allowlist_and_sdk_deny_are_intersections() {
        let registry = ToolRegistry::default();
        registry.register(tool("read_file", "filesystem")).unwrap();
        let config = crate::metadata::object([(
            "managedTools",
            crate::metadata::object([
                ("allow", JsValue::Array(Vec::new())),
                ("deny", JsValue::Array(Vec::new())),
            ]),
        )]);
        let scoped = scope(
            registry.clone(),
            config.as_object().unwrap(),
            Some(Vec::new()),
            None,
        )
        .unwrap();
        assert!(scoped.list().unwrap().is_empty());
        scoped.dispose();
        registry.dispose();
    }
    #[test]
    fn sandbox_vocabulary_and_required_restriction_match_gateway_preflight() {
        for raw in [
            r#"{}"#,
            r#"{"type":"tool_policy"}"#,
            r#"{"filesystem":"workspace_write"}"#,
            r#"{"network":"allow"}"#,
            r#"{"process":null}"#,
            r#"{"filesystem":"read_only","extra":false}"#,
        ] {
            let sandbox = pilotdeck_protocol::parse_json(raw).unwrap();
            let config = crate::metadata::object([("sandbox", sandbox)]);
            let error = validate_sandbox(&config).unwrap_err();
            assert_eq!(error.code, "INVALID_SDK_SESSION_CONFIG");
            assert_eq!(
                error.message,
                "sandbox must contain supported tool_policy restrictions."
            );
        }
        for raw in [
            r#"{"filesystem":"read_only"}"#,
            r#"{"type":"tool_policy","network":"deny"}"#,
            r#"{"process":"deny"}"#,
        ] {
            assert!(
                validate_sandbox(&crate::metadata::object([(
                    "sandbox",
                    pilotdeck_protocol::parse_json(raw).unwrap()
                )]))
                .is_ok()
            );
        }
    }
    #[test]
    fn fixed_ts_private_policy_oracle_matches_the_actual_scoped_catalog() {
        let fixture = pilotdeck_protocol::parse_json(include_str!(
            "../../tests/fixtures/sdk-tool-policy/oracle.json"
        ))
        .unwrap();
        let Some(JsValue::Array(cases)) = fixture.get("cases") else {
            panic!("oracle cases");
        };
        assert_eq!(cases.len(), 16);
        for case in cases {
            let input = case.get("input").unwrap();
            let name = case.get("name").unwrap();
            let upstream = input
                .get("organization")
                .filter(|value| crate::runtime::turn::submit::truthy(value))
                .map(|value| {
                    (
                        strings(value.get("allow")).unwrap_or_default(),
                        strings(value.get("deny")).unwrap_or_default(),
                    )
                });
            let base_policy = Policy {
                managed: upstream,
                sandbox: None,
                allowed: None,
                denied: None,
            };
            let registry = ToolRegistry::new(
                None,
                Some(Arc::new(move |tool| base_policy.allows(tool.fields()))),
            );
            let Some(JsValue::Array(tools)) = input.get("tools") else {
                panic!("oracle tools");
            };
            for tool in tools {
                registry
                    .register(Arc::new(Tool(tool.as_object().unwrap().clone())))
                    .unwrap();
            }
            let explicit = input.get("explicit").unwrap();
            let mut config = JsObject::new();
            for key in ["managed", "sandbox"] {
                if let Some(value) = input.get(key) {
                    config.insert(
                        if key == "managed" {
                            "managedTools"
                        } else {
                            key
                        }
                        .into(),
                        value.clone(),
                    );
                }
            }
            let scoped = scope(
                registry.clone(),
                &config,
                strings(explicit.get("allowedTools")),
                strings(explicit.get("disallowedTools")),
            )
            .unwrap();
            let Some(JsValue::Array(blocked)) = case.get("blocked") else {
                panic!("oracle blocked");
            };
            for tool in tools {
                let tool_name = tool.get("name").unwrap().as_string().unwrap();
                assert_eq!(
                    scoped.get(tool_name).is_some(),
                    !blocked.contains(&JsValue::String(tool_name.clone())),
                    "case={name:?} tool={tool_name:?}"
                );
            }
            scoped.dispose();
            registry.dispose();
        }
    }
}
