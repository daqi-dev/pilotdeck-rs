//! Style-only refresh on the existing project generation and session owner.
use super::sdk_adapter::NativeSdkPort;
use crate::{
    native_process::{Options, SessionOverrides},
    runtime::{
        project::{ProjectResources, ProjectRuntimeRegistry, Resource, plugins::PluginResources},
        session::SessionOwner,
        settings::{OutputStylesReload, ReloadOutputStyles, SettingsError, SettingsFuture},
    },
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct NativeStyleReload {
    pub options: Options,
    pub project: Arc<ProjectRuntimeRegistry>,
    pub owner: SessionOwner,
    pub interactions: Arc<crate::runtime::interaction::GatewayInteractionCoordinator>,
    pub sdk: Arc<NativeSdkPort>,
    pub overrides: Arc<SessionOverrides>,
}
impl ReloadOutputStyles for NativeStyleReload {
    fn reload(&self, input: &JsValue) -> SettingsFuture<Result<OutputStylesReload, SettingsError>> {
        let this = self.clone();
        let input = input.clone();
        // The owned task holds both the admission and the generation lease;
        // caller cancellation cannot silently interrupt the closing loop.
        let job = tokio::spawn(async move {
            static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
                std::sync::OnceLock::new();
            let _permit = SLOTS
                .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4)))
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| {
                    SettingsError::plain("Error", "Output style reload is unavailable.")
                })?;
            if let Some(project) = input
                .get("projectKey")
                .and_then(JsValue::as_string)
                .and_then(|s| s.to_utf8().ok())
                && !this.options.serves_project(&project)
            {
                return Err(SettingsError::gateway(
                    "CAPABILITY_UNAVAILABLE",
                    "The isolated native process serves only its configured project.",
                ));
            }
            let project = this.project.clone();
            let lease = tokio::task::spawn_blocking(move || project.acquire(None))
                .await
                .map_err(|e| SettingsError::plain("Error", e.to_string()))?
                .map_err(|e| SettingsError::plain("Error", e.to_string()))?;
            let resource = lease
                .runtime()
                .resources()
                .get::<ProjectResources>()
                .ok_or_else(|| {
                    SettingsError::plain("Error", "Native project resources are missing.")
                })?;
            let plugin = resource
                .context()
                .resource(Resource::PluginRuntime)
                .ok_or_else(|| {
                    SettingsError::plain("Error", "Native plugin runtime is missing.")
                })?;
            let runtime = plugin
                .as_any()
                .downcast_ref::<PluginResources>()
                .ok_or_else(|| {
                    SettingsError::plain("Error", "Native plugin runtime has the wrong type.")
                })?
                .runtime
                .clone();
            let changed = runtime
                .reload_output_styles()
                .await
                .map_err(|e| SettingsError::plain("Error", e.to_string()))?;
            let sdk_sessions: Vec<JsString> = {
                let registry = this.sdk.registry.lock().await;
                registry
                    .project_entries()
                    .into_iter()
                    .filter_map(|(key, _)| {
                        let session = key.as_string()?;
                        registry.session_config(session).map(|_| session.clone())
                    })
                    .collect()
            };
            let sessions = unique_sessions(sdk_sessions, this.overrides.output_style_sessions());
            for session in sessions {
                this.owner
                    .close(&session)
                    .await
                    .map_err(|e| SettingsError::plain("Error", e.message))?;
                this.interactions
                    .close_session(&session, &"sdk_output_styles_reloaded".into());
            }
            Ok(OutputStylesReload {
                changed: changed.into_iter().map(JsString::from).collect(),
            })
        });
        Box::pin(async move {
            job.await
                .map_err(|e| SettingsError::plain("Error", e.to_string()))?
        })
    }
}

fn unique_sessions(sdk: Vec<JsString>, selected: Vec<JsString>) -> Vec<JsString> {
    let mut seen = std::collections::HashSet::new();
    sdk.into_iter()
        .chain(selected)
        .filter(|key| seen.insert(key.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::sdk_session::{SdkSessionProjects, SdkSessionProjectsTable};
    #[test]
    fn same_session_in_real_project_and_style_stores_closes_once() {
        let projects = SdkSessionProjects::shared();
        let selected = SessionOverrides::default();
        let same = JsString::from("same");
        projects.set(&JsValue::String(same.clone()), "/project".into());
        projects.set(&JsValue::String("sdk-only".into()), "/project".into());
        selected.set_output_style(&same, "p:test".into());
        selected.set_output_style(&"style-only".into(), "p:test".into());
        let sdk = projects
            .entries()
            .into_iter()
            .filter_map(|(value, _)| value.as_string().cloned())
            .collect();
        let sessions = unique_sessions(sdk, selected.output_style_sessions());
        assert_eq!(sessions, vec![same, "sdk-only".into(), "style-only".into()]);
    }
}
