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
        _project_key: Option<&JsString>,
    ) -> Result<ResolvedSdkRuntime, SdkSessionError> {
        // Native workspace admission already rejects projects outside the served project.
        let lease = self
            .project
            .acquire(None)
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
    fn set<'a>(
        &'a self,
        session: &'a JsString,
        config: &'a JsValue,
        project: Option<&'a JsString>,
        completion: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<bool, SubmitError>> {
        Box::pin(async move {
            let signal = CompletionSignal(completion);
            self.registry
                .lock()
                .await
                .set_session_config(session, config, project, Some(&signal))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::object;
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
