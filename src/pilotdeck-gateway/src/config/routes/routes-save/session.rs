//! CONFIG/SDK adapter over the existing transactional project-runtime registry.
use super::{ConfigRoutes, Result, RouteError};
use crate::{
    config::{Env, store::ConfigStore},
    runtime::{
        plugins::{PluginRuntime, PluginRuntimeOptions},
        project::{
            DisposeFuture, ProjectRuntimeRegistry, ResourceBundle, RuntimeError, RuntimeResources,
        },
        sdk_session::{ResolvedSdkRuntime, SdkRuntimeResolver, SdkSessionError},
    },
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};
struct Resources {
    snapshot: JsValue,
    plugin: Arc<PluginRuntime>,
}
impl RuntimeResources for Resources {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async {
            self.plugin
                .dispose()
                .await
                .map_err(|e| RuntimeError::new(e.message))
        })
    }
}
pub(super) struct Resolver {
    home: PathBuf,
    cwd: PathBuf,
    env: Env,
    store: ConfigStore,
    pub(super) builtin_plugins: Vec<Arc<crate::runtime::plugins::LoadedPlugin>>,
    builtin_skills_root: Option<PathBuf>,
    registry: OnceLock<Arc<ProjectRuntimeRegistry>>,
    refresh: OnceLock<crate::runtime::refresh::RegistryRefresh>,
}
impl Resolver {
    pub fn new(
        home: PathBuf,
        cwd: PathBuf,
        env: Env,
        store: ConfigStore,
        builtin_plugins: Vec<Arc<crate::runtime::plugins::LoadedPlugin>>,
        builtin_skills_root: Option<PathBuf>,
    ) -> Self {
        Self {
            home,
            cwd,
            env,
            store,
            builtin_plugins,
            builtin_skills_root,
            registry: OnceLock::new(),
            refresh: OnceLock::new(),
        }
    }
    fn registry(&self) -> &Arc<ProjectRuntimeRegistry> {
        self.registry.get_or_init(|| {
            let home = self.home.clone();
            let cwd = self.cwd.clone();
            let env = self.env.clone();
            let store = self.store.clone();
            let builtin_plugins = self.builtin_plugins.clone();
            let builtin_skills_root = self.builtin_skills_root.clone();
            Arc::new(ProjectRuntimeRegistry::new(
                self.cwd.clone(),
                Arc::new(move |root: &Path, resources: &mut ResourceBundle| {
                    let snapshot = if root == cwd {
                        (*store.snapshot()).clone()
                    } else {
                        crate::config::load_snapshot(
                            &home,
                            &cwd,
                            &env,
                            Some(&root.to_string_lossy()),
                            1.0,
                            &super::timestamp(),
                        )
                        .map_err(|e| {
                            RuntimeError::new(String::from_utf16_lossy(e.message.units()))
                        })?
                    };
                    let enabled = snapshot
                        .get("config")
                        .and_then(|v| v.get("extension"))
                        .and_then(|v| v.get("builtinPluginsEnabled"))
                        .and_then(JsValue::as_object)
                        .map(|o| {
                            o.entries()
                                .iter()
                                .filter_map(|(k, v)| Some((k.to_utf8().ok()?, v.as_bool()?)))
                                .collect()
                        })
                        .unwrap_or_default();
                    let plugin = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
                        project_root: root.into(),
                        pilot_home: home.clone(),
                        builtin_skills_root: builtin_skills_root.clone(),
                        builtin_plugins: builtin_plugins.clone(),
                        builtin_plugins_enabled: enabled,
                    }));
                    resources.add(Resources { snapshot, plugin });
                    Ok(())
                }),
            ))
        })
    }
    pub(super) fn invalidate(&self, key: Option<&JsString>) {
        let root = key.map(|s| PathBuf::from(String::from_utf16_lossy(s.units())));
        self.registry().invalidate(root.as_deref());
    }
    pub(super) fn queue_reload(
        &self,
    ) -> std::result::Result<
        crate::runtime::refresh::BoxFuture<crate::runtime::project::RuntimeResult>,
        RuntimeError,
    > {
        use crate::runtime::refresh::RefreshRegistry;
        self.refresh
            .get_or_init(|| crate::runtime::refresh::RegistryRefresh::new(self.registry().clone()))
            .reload()
    }
}
impl SdkRuntimeResolver for Resolver {
    fn resolve(
        &self,
        key: Option<&JsString>,
    ) -> std::result::Result<ResolvedSdkRuntime, SdkSessionError> {
        let root = key
            .filter(|s| !s.is_empty())
            .map(|s| crate::metadata::resolve(&self.cwd, &String::from_utf16_lossy(s.units())));
        let runtime = self
            .registry()
            .resolve(root.as_deref())
            .map_err(|e| SdkSessionError::new("gateway_request_failed", e.message))?;
        let resources = runtime
            .resources()
            .get::<Resources>()
            .expect("CONFIG resources");
        Ok(ResolvedSdkRuntime {
            project_root: runtime.project_root().into(),
            snapshot: resources.snapshot.clone(),
            plugin_runtime: resources.plugin.clone(),
        })
    }
}
impl ConfigRoutes {
    pub(super) fn check_busy(&self, key: &JsString, message: &str) -> Result<()> {
        if self
            .session_owner
            .as_ref()
            .is_some_and(|owner| owner.has_active_turn(key))
        {
            return Err(RouteError::new("SESSION_BUSY", message));
        }
        Ok(())
    }
    pub(super) async fn gateway_close_session(&self, key: &JsString, reason: &str) -> Result<()> {
        self.close_session(key).await?;
        if let Some(interactions) = &self.interactions {
            interactions.close_session(key, &reason.into());
        }
        self.permission_modes.lock().unwrap().clear(key);
        self.dialog_project_keys.lock().unwrap().remove(key);
        Ok(())
    }
    pub(super) async fn close_session(&self, key: &JsString) -> Result<()> {
        if let Some(owner) = &self.session_owner {
            let had_session = owner.current_handle(key).is_some();
            owner
                .close(key)
                .await
                .map_err(|e| RouteError::new("gateway_request_failed", e.message))?;
            if had_session {
                self.permission_modes.lock().unwrap().clear(key);
            }
        }
        Ok(())
    }
}

/// Trusted session-construction input, not a wire projection (it includes config).
pub struct SessionInput {
    pub snapshot: JsValue,
    pub sdk_config: Option<JsValue>,
    pub session_plugins: Vec<std::sync::Arc<crate::runtime::plugins::LoadedPlugin>>,
    pub thinking: Option<JsValue>,
    pub overrides: Option<crate::config::session_value::SessionValue>,
    pub output_style_content: Option<String>,
}
impl ConfigRoutes {
    pub(super) async fn prepare_session_input(
        &mut self,
        key: &JsString,
        project: Option<&JsString>,
    ) -> Result<SessionInput> {
        let shared = self.shared_sdk.clone();
        let mut sdk = match &shared {
            Some(shared) => Some(shared.lock().await),
            None => None,
        };
        if let Some(sdk) = &mut sdk {
            std::mem::swap(&mut self.sdk, sdk);
        }
        let result = self.prepare_owned_session_input(key, project).await;
        if let Some(sdk) = &mut sdk {
            std::mem::swap(&mut self.sdk, sdk);
        }
        result
    }
    pub(super) async fn prepare_owned_session_input(
        &mut self,
        key: &JsString,
        project: Option<&JsString>,
    ) -> Result<SessionInput> {
        let before = self.store.snapshot();
        let after = self
            .store
            .reload("turn-start")
            .await
            .map_err(|e| RouteError::from(e.as_ref()))?;
        self.config_changed(&crate::config::changes::diff_config_snapshots(
            &before, &after,
        ))
        .await;
        self.settle_runtime_refresh().await;
        let runtime = self
            .resolver
            .resolve(project)
            .map_err(|e| RouteError::new(e.code, e.message))?;
        let sdk_config = self
            .sdk
            .resolve_session_config(key, &runtime)
            .map_err(|e| RouteError::new(e.code, e.message))?;
        let session_plugins = self.sdk.session_plugins(key).to_vec();
        let output_style_content = self.session_output_style_content(key, project).await?;
        Ok(SessionInput {
            snapshot: runtime.snapshot,
            sdk_config,
            session_plugins,
            thinking: self
                .sdk
                .thinking_override(key)
                .filter(|v| **v != JsValue::Null)
                .cloned(),
            overrides: self
                .sdk
                .session_overrides()
                .and_then(|overrides| overrides.get(key)),
            output_style_content,
        })
    }
}
