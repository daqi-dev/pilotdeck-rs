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
            if matches!(input, JsValue::Null) {
                return Err(SettingsError::plain(
                    "TypeError",
                    "Cannot read properties of null (reading 'projectKey')",
                ));
            }
            let selected = selected_root(&this.options, &input)?;
            let filter_project = input.get("projectKey").is_some_and(crate::runtime::turn::submit::truthy);
            let project = this.project.clone();
            let acquire_root = selected.clone();
            let lease = tokio::task::spawn_blocking(move || project.acquire(Some(&acquire_root)))
                .await
                .map_err(|e| SettingsError::plain("Error", e.to_string()))?
                .map_err(|e| SettingsError::plain("Error", e.to_string()))?;
            let selected_key = JsString::from(selected.to_string_lossy().as_ref());
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
                    .filter_map(|(key, project)| {
                        if filter_project && project != selected_key { return None; }
                        let session = key.as_string()?;
                        registry.session_config(session).map(|_| session.clone())
                    })
                    .collect()
            };
            let selected_sessions = this.overrides.output_style_sessions().into_iter().filter(|session| {
                !filter_project || this.owner.project_key(session).as_ref() == Some(&selected_key)
            }).collect();
            let sessions = unique_sessions(sdk_sessions, selected_sessions);
            for session in sessions {
                // Prevent a disposed key from being recreated for another project
                // between project selection and the asynchronous close.
                let scope = this.owner.reserve_project(
                    &session,
                    filter_project.then_some(&selected_key),
                    &selected_key,
                );
                let _scope = match scope {
                    Ok(scope) => scope,
                    Err(error) if error.message.starts_with("PROJECT_SESSION_MISMATCH:") => continue,
                    Err(error) => return Err(SettingsError::plain("Error", error.message)),
                };
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

fn selected_root(options: &Options, input: &JsValue) -> Result<std::path::PathBuf, SettingsError> {
    match input.get("projectKey") {
        None | Some(JsValue::Null | JsValue::Undefined) => Ok(options.project.clone()),
        Some(JsValue::String(key)) => crate::metadata::resolve_process_identity(std::path::Path::new(""), key)
            .map_err(|e| SettingsError::plain("Error", e.to_string()))?
            .map(std::path::PathBuf::from)
            .ok_or_else(|| SettingsError::gateway("CAPABILITY_UNAVAILABLE", "Project path cannot be represented by this host.")),
        Some(value) => {
            let received = match crate::metadata::argument_received(value) {
                crate::metadata::ArgumentReceived::Text(text) => text,
                crate::metadata::ArgumentReceived::Throws { message, .. } => return Err(SettingsError::plain("TypeError", message)),
            };
            let mut units: Vec<u16> = "The \"paths[0]\" argument must be of type string. Received ".encode_utf16().collect();
            units.extend_from_slice(received.units());
            Err(SettingsError::plain("TypeError", JsString::from_utf16(units)))
        }
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
    #[tokio::test]
    async fn explicit_style_reload_preserves_other_project_sessions() {
        use crate::runtime::sdk_session::{SdkSessionOptions, SdkSessionRegistry};
        use crate::runtime::session::SessionContext;
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let a = requests.options.project.clone();
        let b = a.parent().unwrap().join("style-B");
        std::fs::create_dir_all(&b).unwrap();
        let mut b_options = requests.options.clone();
        b_options.project = b.clone();
        crate::native_process::assembly::register_project(&b_options).unwrap();
        let a_key = JsString::from(a.to_string_lossy().as_ref());
        let b_key = JsString::from(b.to_string_lossy().as_ref());
        requests.owner.enforce_project_binding();
        for (session, project) in [("style-a", &a_key), ("style-b", &b_key)] {
            requests.owner.get_or_create(SessionContext {
                session_key: session.into(), project_key: Some(project.clone()),
                channel_key: "test".into(), allowed_tools: None, disallowed_tools: None,
            }).await.unwrap();
        }
        let overrides = Arc::new(SessionOverrides::default());
        overrides.set_output_style(&"style-a".into(), "a".into());
        overrides.set_output_style(&"style-b".into(), "b".into());
        let sdk = Arc::new(NativeSdkPort {
            registry: tokio::sync::Mutex::new(SdkSessionRegistry::new(SdkSessionOptions::new(
                a.clone(), a.clone(), requests.env.clone(),
                Arc::new(super::super::sdk_adapter::NativeSdkResolver { project: requests.project.clone() }),
            ))),
            handlers: Arc::default(), async_hooks: Arc::default(), mcp_config: Arc::default(),
        });
        let reload = NativeStyleReload {
            options: requests.options.clone(), project: requests.project.clone(), owner: requests.owner.clone(),
            interactions: Arc::new(crate::runtime::interaction::GatewayInteractionCoordinator::new(Default::default())),
            sdk, overrides,
        };
        let input = crate::metadata::object([("projectKey", JsValue::String(b_key))]);
        reload.reload(&input).await.unwrap();
        assert!(requests.owner.current_handle(&"style-a".into()).is_some(), "B reload closed A session");
        assert!(requests.owner.current_handle(&"style-b".into()).is_none(), "B reload did not close B session");
        // Modify only B after its initial staging. Selecting the fallback A
        // would return no change, so this proves the actual plugin consumer.
        let plugin = b.join(".pilotdeck/plugins/style-plugin");
        std::fs::create_dir_all(plugin.join("output-styles")).unwrap();
        std::fs::write(plugin.join("plugin.json"), r#"{"name":"style-plugin"}"#).unwrap();
        std::fs::write(plugin.join("output-styles/project-b.md"), "Only B style").unwrap();
        let changed = reload.reload(&input).await.unwrap();
        assert_eq!(changed.changed, vec![JsString::from("style-plugin:project-b")]);
        // The fixed TS loop intentionally closes all SDK-selected sessions
        // when projectKey is omitted, even though only the fallback reloads.
        reload.reload(&crate::metadata::object([])).await.unwrap();
        assert!(requests.owner.current_handle(&"style-a".into()).is_none());
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }

}
