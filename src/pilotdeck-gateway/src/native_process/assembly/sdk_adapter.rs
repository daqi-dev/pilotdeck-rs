//! Incremental native SDK session adapter. Only permissionMode is enabled until
//! each additional SDK setting has a native consumer; unknown fields fail closed.
use std::sync::Arc;

use pilotdeck_protocol::{JsString, JsValue};
use tokio::sync::Mutex;

use crate::runtime::{
    project::{ProjectResources, ProjectRuntimeRegistry, Resource, plugins::PluginResources},
    sdk_session::{
        ResolvedSdkRuntime, SdkAbortSignal, SdkRuntimeResolver, SdkSessionError, SdkSessionRegistry,
    },
    session::TurnCompletionHandle,
    turn::{
        admission::SubmitSdkSessionConfig,
        submit::{SubmitError, SubmitFuture},
    },
};

/// Resolves from the same project generation used by native session prepare.
/// The returned lease outlives plugin refresh and SDK config resolution.
pub(super) struct NativeSdkResolver {
    pub project: Arc<ProjectRuntimeRegistry>,
}
impl SdkRuntimeResolver for NativeSdkResolver {
    fn resolve(
        &self,
        project_key: Option<&JsString>,
    ) -> Result<ResolvedSdkRuntime, SdkSessionError> {
        let root = project_key
            .map(|key| super::super::project_scope::resolve(key))
            .transpose()
            .map_err(|error| SdkSessionError::raw("Error", String::from_utf16_lossy(error.message.units())))?;
        let lease = self
            .project
            .acquire(root.as_deref())
            .map_err(|e| SdkSessionError::raw("Error", e.to_string()))?;
        let root = lease.runtime().project_root().to_path_buf();
        let resources = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .ok_or_else(|| {
                SdkSessionError::raw("Error", "Native project resources are missing.")
            })?;
        let staged = resources.context();
        let snapshot = staged.snapshot().as_ref().clone();
        let plugin = staged
            .resource(Resource::PluginRuntime)
            .ok_or_else(|| SdkSessionError::raw("Error", "Native plugin runtime is missing."))?;
        let plugin_runtime = plugin
            .as_any()
            .downcast_ref::<PluginResources>()
            .ok_or_else(|| {
                SdkSessionError::raw("Error", "Native plugin runtime has the wrong type.")
            })?
            .runtime
            .clone();
        Ok(ResolvedSdkRuntime {
            project_root: root,
            snapshot,
            plugin_runtime,
            lease: Some(lease),
        })
    }
}

struct CompletionSignal<'a>(&'a TurnCompletionHandle);
impl SdkAbortSignal for CompletionSignal<'_> {
    fn throw_if_aborted(&self) -> Result<(), SdkSessionError> {
        if self.0.is_cancelled() {
            Err(SdkSessionError::raw(
                "AbortError",
                "The operation was aborted.",
            ))
        } else {
            Ok(())
        }
    }
}

pub(super) struct NativeSdkPort {
    pub registry: Mutex<SdkSessionRegistry>,
    pub handlers: Arc<std::sync::Mutex<crate::runtime::sdk_session::SessionHandlerRegistry>>,
    pub async_hooks: crate::runtime::turn::interaction::SdkAsyncRegistrySlot,
    pub mcp_config: Arc<std::sync::OnceLock<Arc<crate::runtime::mcp::sdk::SdkMcpConfig>>>,
}
impl NativeSdkPort {
    /// Read the committed value, including the session's retained configuration
    /// when the current request did not supply a new SDK config.
    pub async fn prompt_config(&self, session: &JsString) -> Option<JsValue> {
        self.registry.lock().await.session_config(session).cloned()
    }
    pub async fn permission_mode(&self, session: &JsString) -> Option<JsString> {
        self.registry
            .lock()
            .await
            .session_config(session)
            .and_then(|v| v.get("permissionMode"))
            .and_then(JsValue::as_string)
            .cloned()
    }
}
impl SubmitSdkSessionConfig for NativeSdkPort {
    fn validate(&self, config: &JsValue) -> Result<(), SubmitError> {
        validate_supported_config(config)
    }
    fn permission_mode<'a>(&'a self, session: &'a JsString) -> SubmitFuture<'a, Option<JsString>> {
        Box::pin(NativeSdkPort::permission_mode(self, session))
    }
    fn hooks<'a>(
        &'a self,
        session: &'a JsString,
    ) -> SubmitFuture<
        'a,
        Result<Option<crate::runtime::turn::interaction::SdkHookState>, SubmitError>,
    > {
        Box::pin(async move {
            let config = self
                .prompt_config(session)
                .await
                .unwrap_or(JsValue::Undefined);
            let active = config
                .get("hooks")
                .is_some_and(|value| !value.is_undefined());
            Ok(Some(crate::runtime::turn::interaction::SdkHookState {
                settings: super::sdk_hooks::settings(&config)?,
                active,
                include_events: config.get("includeHookEvents") == Some(&JsValue::Bool(true)),
                handlers: self.handlers.clone(),
                async_registry: self.async_hooks.clone(),
                current_turn: None,
                native_steer_bound: None,
            }))
        })
    }
    fn set<'a>(
        &'a self,
        session: &'a JsString,
        config: &'a JsValue,
        project: Option<&'a JsString>,
        completion: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<bool, SubmitError>> {
        Box::pin(async move {
            let signal = CompletionSignal(completion);
            let mut registry = self.registry.lock().await;
            let normalized =
                resolve_native_agents(&registry, session, config, project, self.mcp_config.get())
                    .await
                    .map_err(sdk_submit_error)?;
            signal.throw_if_aborted().map_err(sdk_submit_error)?;
            registry
                .set_session_config(session, &normalized, project, Some(&signal))
                .await
                .map_err(|e| {
                    SubmitError::new(
                        if e.code.is_empty() {
                            "gateway_request_failed"
                        } else {
                            &e.code
                        },
                        e.message,
                    )
                })
        })
    }
}

pub(super) fn apply_prompt_config(target: &mut pilotdeck_protocol::JsObject, sdk: &JsValue) {
    if let Some(agents) = sdk.get("agents").and_then(JsValue::as_object) {
        target.insert(
            "subagentDefinitions".into(),
            JsValue::Object(project_agents(agents)),
        );
    }
    if let Some(skills) = sdk.get("skills").filter(|value| !value.is_undefined()) {
        target.insert("nativeSkillSelection".into(), skills.clone());
    }
    for key in ["systemPrompt", "appendSystemPrompt", "planModeInstructions"] {
        if let Some(value) = sdk.get(key).filter(|value| !value.is_undefined()) {
            target.insert(key.into(), value.clone());
        }
    }
}

fn validate_supported_config(config: &JsValue) -> Result<(), SubmitError> {
    let object = config.as_object().ok_or_else(|| {
        SubmitError::new(
            "INVALID_SDK_SESSION_CONFIG",
            "sdkSessionConfig must be an object.",
        )
    })?;
    super::sdk_hooks::validate(config)?;
    validate_agents(object.get("agents"))?;
    if let Some(skills) = object.get("skills").filter(|value| !value.is_undefined())
        && !valid_skill_selector(skills)
    {
        return Err(SubmitError::new(
            "INVALID_SDK_SESSION_CONFIG",
            "skills must be an array of strings or all.",
        ));
    }
    for key in ["systemPrompt", "appendSystemPrompt", "planModeInstructions"] {
        if object
            .get(key)
            .is_some_and(|value| !value.is_undefined() && !matches!(value, JsValue::String(_)))
        {
            return Err(SubmitError::new(
                "INVALID_SDK_SESSION_CONFIG",
                format!("{key} must be a string."),
            ));
        }
    }
    if object.entries().iter().any(|(key, value)| {
        ![
            "permissionMode",
            "hooks",
            "includeHookEvents",
            "agents",
            "skills",
            "systemPrompt",
            "appendSystemPrompt",
            "planModeInstructions",
        ]
        .iter()
        .any(|name| key.equals_str(name))
            && !value.is_undefined()
    }) {
        return Err(SubmitError::new(
            "CAPABILITY_UNAVAILABLE",
            "This SDK session setting is unavailable in the native host.",
        ));
    }
    if let Some(value) = object.get("permissionMode").filter(|v| !v.is_undefined())
        && !value
            .as_string()
            .is_some_and(|s| s.equals_str("acceptEdits") || s.equals_str("dontAsk"))
    {
        return Err(SubmitError::new(
            "INVALID_SDK_SESSION_CONFIG",
            "permissionMode must be acceptEdits or dontAsk.",
        ));
    }
    Ok(())
}

fn sdk_submit_error(error: SdkSessionError) -> SubmitError {
    SubmitError::new(
        if error.code.is_empty() {
            "gateway_request_failed"
        } else {
            &error.code
        },
        error.message,
    )
}
fn valid_skill_selector(value: &JsValue) -> bool {
    value.as_string().is_some_and(|v| v.equals_str("all"))
        || matches!(value,JsValue::Array(items) if items.iter().all(|item|item.as_string().is_some()))
}
fn validate_agents(value: Option<&JsValue>) -> Result<(), SubmitError> {
    let Some(value) = value.filter(|value| !value.is_undefined()) else {
        return Ok(());
    };
    let agents = value.as_object().ok_or_else(|| {
        SubmitError::new("INVALID_SDK_SESSION_CONFIG", "agents must be an object.")
    })?;
    if agents.entries().len() > 64 {
        return Err(SubmitError::new(
            "CAPABILITY_UNAVAILABLE",
            "Native SDK supports at most 64 agent definitions.",
        ));
    }
    for (id, definition) in agents.entries() {
        let invalid = || {
            SubmitError::new(
                "INVALID_SDK_SESSION_CONFIG",
                format!(
                    "Invalid SDK agent definition: {}.",
                    String::from_utf16_lossy(id.units())
                ),
            )
        };
        if id.is_empty() {
            return Err(invalid());
        }
        let fields = definition.as_object().ok_or_else(invalid)?;
        for name in ["description", "prompt"] {
            if fields.get(name).and_then(JsValue::as_string).is_none() {
                return Err(invalid());
            }
        }
        for (name, value) in fields.entries() {
            if value.is_undefined() {
                continue;
            }
            if [
                "description",
                "prompt",
                "model",
                "initialPrompt",
                "observer",
                "observerMessage",
                "criticalSystemReminder_EXPERIMENTAL",
            ]
            .iter()
            .any(|key| name.equals_str(key))
            {
                if value.as_string().is_none() {
                    return Err(invalid());
                }
            } else if name.equals_str("tools") || name.equals_str("disallowedTools") {
                if !matches!(value,JsValue::Array(items) if items.iter().all(|item|item.as_string().is_some()))
                {
                    return Err(invalid());
                }
            } else if name.equals_str("skills") {
                if !valid_skill_selector(value) {
                    return Err(invalid());
                }
            } else if name.equals_str("memory") {
                if !value.as_string().is_some_and(|value| {
                    value.equals_str("inherit") || value.equals_str("disabled")
                }) {
                    return Err(invalid());
                }
            } else if name.equals_str("permissionMode") {
                if !value.as_string().is_some_and(|value| {
                    ["default", "plan", "bypassPermissions"]
                        .iter()
                        .any(|name| value.equals_str(name))
                }) {
                    return Err(invalid());
                }
            } else if name.equals_str("effort") {
                if !value.as_string().is_some_and(|value| {
                    ["low", "medium", "high"]
                        .iter()
                        .any(|name| value.equals_str(name))
                }) {
                    return Err(invalid());
                }
            } else if name.equals_str("maxTurns") {
                if !value.as_number().is_some_and(|value| {
                    value.is_finite()
                        && value > 0.0
                        && value.fract() == 0.0
                        && value <= 9_007_199_254_740_991.0
                }) {
                    return Err(invalid());
                }
            } else if name.equals_str("mcpServers") {
                super::super::agent_mcp::validate(value).map_err(sdk_submit_error)?;
            } else if name.equals_str("background") && matches!(value, JsValue::Bool(_)) {
            } else {
                return Err(SubmitError::new(
                    "CAPABILITY_UNAVAILABLE",
                    format!(
                        "Native SDK agent field {} is unavailable.",
                        String::from_utf16_lossy(name.units())
                    ),
                ));
            }
        }
    }
    Ok(())
}
fn project_agents(agents: &pilotdeck_protocol::JsObject) -> pilotdeck_protocol::JsObject {
    let mut output = pilotdeck_protocol::JsObject::new();
    for (id, agent) in agents.entries() {
        let mut definition = pilotdeck_protocol::JsObject::new();
        definition.insert("id".into(), JsValue::String(id.clone()));
        for (source, target) in [
            ("description", "description"),
            ("prompt", "systemPromptSuffix"),
        ] {
            if let Some(value) = agent.get(source) {
                definition.insert(target.into(), value.clone());
            }
        }
        definition.insert(
            "allowedTools".into(),
            agent
                .get("tools")
                .cloned()
                .unwrap_or_else(|| JsValue::Array(vec!["*".into()])),
        );
        if let Some(JsValue::Array(items)) = agent.get("disallowedTools")
            && !items.is_empty()
        {
            definition.insert("disallowedTools".into(), JsValue::Array(items.clone()));
        }
        definition.insert("omitProjectInstructions".into(), JsValue::Bool(false));
        definition.insert("omitGitStatus".into(), JsValue::Bool(false));
        definition.insert(
            "isReadOnly".into(),
            JsValue::Bool(
                agent
                    .get("permissionMode")
                    .and_then(JsValue::as_string)
                    .is_some_and(|value| value.equals_str("plan")),
            ),
        );
        if let Some(model) = agent
            .get("model")
            .and_then(JsValue::as_string)
            .and_then(|value| value.to_utf8().ok())
            && let Some((provider, model)) = model.split_once('/')
            && !provider.is_empty()
            && !model.is_empty()
        {
            let mut route = pilotdeck_protocol::JsObject::new();
            route.insert("provider".into(), provider.into());
            route.insert("model".into(), model.into());
            definition.insert("modelOverride".into(), JsValue::Object(route));
        }
        for (source, target) in [
            ("mcpServers", "mcpServers"),
            ("maxTurns", "maxTurns"),
            ("effort", "effort"),
            ("permissionMode", "permissionMode"),
            ("skills", "skills"),
            ("memory", "memory"),
            ("initialPrompt", "initialPrompt"),
            ("background", "background"),
            ("observer", "observer"),
            ("observerMessage", "observerMessage"),
            (
                "criticalSystemReminder_EXPERIMENTAL",
                "criticalSystemReminder",
            ),
        ] {
            if let Some(value) = agent.get(source).filter(|value| !value.is_undefined()) {
                if source == "background" && value != &JsValue::Bool(true) {
                    continue;
                }
                definition.insert(target.into(), value.clone());
            }
        }
        output.insert(id.clone(), JsValue::Object(definition));
    }
    output
}
async fn resolve_native_agents(
    registry: &SdkSessionRegistry,
    session: &JsString,
    config: &JsValue,
    project: Option<&JsString>,
    mcp_config: Option<&Arc<crate::runtime::mcp::sdk::SdkMcpConfig>>,
) -> Result<JsValue, SdkSessionError> {
    use crate::runtime::sdk_session::{
        ModelField, assert_managed_model_allowed, resolve_configured_model, resolve_session_skills,
    };
    if config.get("agents").is_none_or(JsValue::is_undefined) {
        return Ok(config.clone());
    }
    let runtime = registry.options().resolver.resolve(project)?;
    runtime
        .plugin_runtime
        .refresh()
        .await
        .map_err(|error| SdkSessionError::raw("Error", error.to_string()))?;
    let view = runtime.plugin_runtime.create_view(vec![]).await;
    let available = view.all_skills();
    let parent = match config.get("skills") {
        Some(JsValue::Array(names)) => Some(resolve_session_skills(names, &available)?),
        _ => None,
    };
    let mut next = config.as_object().cloned().unwrap_or_default();
    let mut agents = config
        .get("agents")
        .and_then(JsValue::as_object)
        .cloned()
        .unwrap_or_default();
    for (id, agent) in agents.entries().to_vec() {
        let mut agent = agent.as_object().cloned().unwrap_or_default();
        if let Some(requested) = agent.get("model").and_then(JsValue::as_string) {
            let model = resolve_configured_model(&runtime.snapshot, requested, ModelField::Model)
                .map_err(|_| {
                SdkSessionError::new(
                    "INVALID_SDK_AGENT_MODEL",
                    format!(
                        "Agent {} model is not uniquely resolvable from the Gateway catalog: {}",
                        String::from_utf16_lossy(id.units()),
                        String::from_utf16_lossy(requested.units())
                    ),
                )
            })?;
            registry.assert_model_allowed(session, Some(&model), project)?;
            assert_managed_model_allowed(config.get("managedModels"), &model)?;
            let mut text = model.provider.units().to_vec();
            text.push(47);
            text.extend_from_slice(model.model.units());
            agent.insert("model".into(), JsValue::String(JsString::from_utf16(text)));
        }
        let selected = match agent.get("skills") {
            Some(JsValue::Array(names)) => Some(resolve_session_skills(names, &available)?),
            Some(JsValue::String(all)) if all.equals_str("all") => parent.clone(),
            None | Some(JsValue::Undefined) => parent.clone(),
            _ => None,
        };
        if let Some(JsValue::Array(selected)) = &selected
            && let Some(JsValue::Array(parent)) = &parent
            && let Some(escaped) = selected.iter().find(|name| !parent.contains(name))
        {
            return Err(SdkSessionError::new(
                "SDK_AGENT_SKILL_OUT_OF_SCOPE",
                format!(
                    "Agent {} skill is not visible to the parent SDK session: {}",
                    String::from_utf16_lossy(id.units()),
                    escaped
                        .as_string()
                        .map(|value| String::from_utf16_lossy(value.units()))
                        .unwrap_or_default()
                ),
            ));
        }
        if let Some(selected) = selected {
            agent.insert("skills".into(), selected);
        }
        if let Some(specs) = agent
            .get("mcpServers")
            .filter(|value| !value.is_undefined())
        {
            let session_servers = mcp_config
                .map(|mcp| mcp.session_mcp_servers(&JsValue::String(session.clone())))
                .unwrap_or_default();
            let resolved = super::super::agent_mcp::resolve(&id, specs, &session_servers)?;
            agent.insert("mcpServers".into(), JsValue::Object(resolved));
        }
        agents.insert(id, JsValue::Object(agent));
    }
    next.insert("agents".into(), JsValue::Object(agents));
    Ok(JsValue::Object(next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::object;
    #[test]
    fn actual_ts_sdk_agent_definition_projection_matches() {
        let oracle =
            pilotdeck_protocol::parse_json(include_str!("../agent_sdk_oracle.json")).unwrap();
        let JsValue::Array(cases) = oracle.get("cases").unwrap() else {
            panic!("cases")
        };
        let mut count = 0;
        for case in cases {
            let sdk = case.get("sdk").unwrap();
            let mut projected = pilotdeck_protocol::JsObject::new();
            apply_prompt_config(&mut projected, sdk);
            if case.get("id") == Some(&"cap-turns".into()) {
                continue;
            } // Organization cap composition belongs to its owner, not this unconfigured native profile.
            assert_eq!(
                projected.get("subagentDefinitions"),
                case.get("definitions"),
                "{:?}",
                case.get("id")
            );
            count += 1;
        }
        assert_eq!(count, 6);
    }
    #[test]
    fn custom_agent_fields_fail_closed_until_their_real_owner_exists() {
        for field in [
            "nativeObserver",
            "nativeMemoryDisabled",
            "unconfiguredExtension",
        ] {
            let mut agent = crate::metadata::object([
                ("description", "fixture".into()),
                ("prompt", "self-written".into()),
            ])
            .as_object()
            .unwrap()
            .clone();
            agent.insert(field.into(), "unsupported".into());
            assert_eq!(
                validate_agents(Some(&crate::metadata::object([(
                    "fixture",
                    JsValue::Object(agent)
                )])))
                .unwrap_err()
                .code,
                "CAPABILITY_UNAVAILABLE"
            );
        }
        for turns in [0.0, -1.0, 1.5, f64::INFINITY] {
            assert!(
                validate_agents(Some(&crate::metadata::object([(
                    "fixture",
                    crate::metadata::object([
                        ("description", "fixture".into()),
                        ("prompt", "self-written".into()),
                        ("maxTurns", JsValue::Number(turns))
                    ])
                )])))
                .is_err()
            );
        }
        assert!(validate_agents(Some(&crate::metadata::object([]))).is_ok());
        assert!(
            validate_agents(Some(&crate::metadata::object([(
                "fixture",
                crate::metadata::object([
                    ("description", "fixture".into()),
                    ("prompt", "self-written".into()),
                    ("tools", JsValue::Array(vec![]))
                ])
            )])))
            .is_ok()
        );
    }

    #[test]
    fn sdk_observer_fields_are_typed_and_preserved_in_the_real_projection() {
        for field in ["observer", "observerMessage"] {
            let mut agent = object([
                ("description", "fixture".into()),
                ("prompt", "self-written".into()),
            ])
            .as_object()
            .unwrap()
            .clone();
            let text = JsValue::String(JsString::from_utf16(vec![0x77, 0xd800]));
            agent.insert(field.into(), text.clone());
            let agents = object([("fixture", JsValue::Object(agent.clone()))]);
            validate_agents(Some(&agents)).unwrap();
            let projected = project_agents(agents.as_object().unwrap());
            assert_eq!(projected.get("fixture").unwrap().get(field), Some(&text));
            for invalid in [JsValue::Null, JsValue::Bool(true), JsValue::Number(1.0)] {
                agent.insert(field.into(), invalid);
                let agents = object([("fixture", JsValue::Object(agent.clone()))]);
                assert_eq!(
                    validate_agents(Some(&agents)).unwrap_err().code,
                    "INVALID_SDK_SESSION_CONFIG"
                );
            }
        }
    }
    #[test]
    fn prompt_projection_preserves_absent_and_accepts_empty_strings() {
        let mut target = pilotdeck_protocol::JsObject::new();
        target.insert("systemPrompt".into(), "base".into());
        apply_prompt_config(&mut target, &object([("appendSystemPrompt", "".into())]));
        assert_eq!(target.get("systemPrompt"), Some(&"base".into()));
        assert_eq!(target.get("appendSystemPrompt"), Some(&"".into()));
        apply_prompt_config(&mut target, &object([("systemPrompt", "new".into())]));
        assert_eq!(target.get("systemPrompt"), Some(&"new".into()));
    }
    #[test]
    fn only_wired_sdk_modes_are_accepted() {
        assert!(validate_supported_config(&object([])).is_ok());
        for mode in ["acceptEdits", "dontAsk"] {
            assert!(validate_supported_config(&object([("permissionMode", mode.into())])).is_ok());
        }
        assert_eq!(
            validate_supported_config(&object([("permissionMode", "plan".into())]))
                .unwrap_err()
                .code,
            "INVALID_SDK_SESSION_CONFIG"
        );
        assert!(validate_supported_config(&object([("systemPrompt", "x".into())])).is_ok());
        assert_eq!(
            validate_supported_config(&object([("systemPrompt", JsValue::Bool(false))]))
                .unwrap_err()
                .code,
            "INVALID_SDK_SESSION_CONFIG"
        );
        assert_eq!(
            validate_supported_config(&object([("sandbox", object([]))]))
                .unwrap_err()
                .code,
            "CAPABILITY_UNAVAILABLE"
        );
    }
}
