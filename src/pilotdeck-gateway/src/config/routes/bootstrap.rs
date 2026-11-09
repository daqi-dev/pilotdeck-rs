//! Default CONFIG composition. The exported owner/factory are the execution
//! host's state handles; constructing a second SDK registry would split controls.
use super::{
    ConfigHost, ConfigRoutes, NativeWeixinFactory, OrganizationPolicy, Result, RouteError,
    SessionInput, WeixinChannelHost,
    native::{SharedConfigRoutes, install, session_input},
};
use crate::{
    config::Env,
    runtime::{
        interaction::GatewayInteractionCoordinator,
        project::{DisposeFuture, RuntimeError, RuntimeResult},
        refresh::BoxFuture,
        session::{
            LiveSession, LiveSessionHandle, LiveTurn, SessionContext, SessionFactory, SessionOwner,
            SessionOwnerOptions,
            sidecar_session::{SidecarSession, SidecarSessionConfig, SidecarSpawner},
        },
    },
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub struct ConfigBootstrap {
    pub state: SharedConfigRoutes,
    pub owner: SessionOwner,
    pub interactions: Arc<GatewayInteractionCoordinator>,
    pub weixin: Arc<WeixinChannelHost>,
    pub weixin_delivery: Arc<super::WeixinDelivery>,
    inputs: Inputs,
}
impl ConfigBootstrap {
    pub fn from_env() -> Result<Self> {
        let env: Env = std::env::vars_os()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect();
        let home = env
            .get("PILOT_HOME")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                RouteError::new(
                    "CONFIG_HOME_REQUIRED",
                    "Native CONFIG requires explicit PILOT_HOME",
                )
            })?;
        let cwd = std::env::current_dir().map_err(io_error)?;
        Self::new(PathBuf::from(home), cwd, env)
    }
    pub fn new(home: PathBuf, cwd: PathBuf, env: Env) -> Result<Self> {
        Self::new_with_channel_gateway(home, cwd, env, None)
    }
    pub fn new_with_channel_gateway(
        home: PathBuf,
        cwd: PathBuf,
        env: Env,
        channel_gateway: Option<Arc<dyn crate::config::channel_commands::ChannelGateway>>,
    ) -> Result<Self> {
        let builtins = builtin_plugins(&cwd, &env)?;
        let skills = resource_root(&cwd, &env, "PILOTDECK_BUNDLED_SKILLS_DIR", "skills");
        let mut routes = ConfigRoutes::new_with_resources(
            home.clone(),
            cwd.clone(),
            env.clone(),
            builtins.clone(),
            skills.clone(),
        )?;
        let policy = policy(&env)?;
        let raw = routes.store.snapshot();
        let user_home = env
            .get("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.parent().map(PathBuf::from))
            .unwrap_or_else(|| home.clone());
        let mut sidecar_env = env.clone();
        for (key, value) in crate::config::webui::runtime_env(
            super::field(&raw, "config"),
            &home,
            &user_home,
            &env,
        ) {
            sidecar_env.insert(key, value);
        }
        let factory =
            NativeWeixinFactory::new(home.clone(), env.get("PILOTDECK_WEIXIN_BASE_URL").cloned())?;
        let factory = match channel_gateway {
            Some(gateway) => factory.with_gateway(gateway),
            None => factory,
        };
        let weixin_delivery = factory.delivery();
        let weixin = Arc::new(WeixinChannelHost::new(Arc::new(factory)));
        let inputs: Inputs = Arc::default();
        let factory = Arc::new(ConfigSessionFactory {
            inputs: inputs.clone(),
            home: home.clone(),
            cwd: cwd.clone(),
            builtin_plugins: builtins,
            builtin_skills_root: skills,
            spawner: Arc::new(BinarySpawner {
                executable: env
                    .get("PILOTDECK_SIDECAR")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_exe()
                            .unwrap_or_default()
                            .with_file_name("pilotdeck-agent-loop")
                    }),
                cwd,
                env: sidecar_env,
            }),
        });
        let owner = SessionOwner::new(SessionOwnerOptions::new(factory.clone()));
        let interactions = Arc::new(GatewayInteractionCoordinator::default());
        let mut host = ConfigHost::new(owner.clone());
        host.organization_policy = policy;
        host.interactions = Some(interactions.clone());
        host.weixin_login = Some(weixin.clone());
        routes.bind_host(host);
        let state = install(routes);
        Ok(Self {
            state,
            owner,
            interactions,
            weixin,
            weixin_delivery,
            inputs,
        })
    }
    /// Turn admission resolves CONFIG before entering the owner's creation gate,
    /// so a concurrent CONFIG close cannot wait on a factory awaiting CONFIG.
    pub async fn session(&self, context: SessionContext) -> Result<LiveSessionHandle> {
        let input = session_input(
            self.state.clone(),
            context.session_key.clone(),
            context.project_key.clone(),
        )
        .await?;
        self.inputs
            .lock()
            .unwrap()
            .insert(context.session_key.clone(), Arc::new(input));
        self.owner
            .get_or_create(context)
            .await
            .map_err(|e| RouteError::new("gateway_request_failed", e.message))
    }
    pub async fn close(&self) -> Result<()> {
        super::native::shutdown(self.state.clone()).await
    }
}
fn io_error(_: std::io::Error) -> RouteError {
    RouteError::new(
        "gateway_request_failed",
        "Cannot read native CONFIG host resources",
    )
}
fn policy(env: &Env) -> Result<OrganizationPolicy> {
    let Some(path) = env.get("PILOTDECK_ORGANIZATION_POLICY_PATH") else {
        return Ok(OrganizationPolicy::default());
    };
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).map_err(io_error)?)
        .map_err(|_| {
            RouteError::new(
                "INVALID_ORGANIZATION_POLICY",
                "Organization policy must be JSON",
            )
        })?;
    let mut sdk = crate::runtime::sdk_session::OrganizationPolicy::default();
    sdk.apply_execution_limits_json(&value)
        .map_err(|error| RouteError::new(error.code, error.message))?;
    let deny_local_settings_updates = match value
        .get("settings")
        .and_then(|s| s.get("canUpdateLocalSettings"))
    {
        None => false,
        Some(serde_json::Value::Bool(false)) => true,
        _ => unreachable!("validated by apply_execution_limits_json"),
    };
    Ok(OrganizationPolicy {
        deny_local_settings_updates,
        sdk,
    })
}
pub(crate) fn resource_root(cwd: &Path, env: &Env, key: &str, relative: &str) -> Option<PathBuf> {
    if let Some(path) = env.get(key).filter(|v| !v.is_empty()) {
        return Some(crate::metadata::resolve(cwd, path));
    }
    let executable = std::env::current_exe().ok()?;
    [
        executable.parent()?.join("resources").join(relative),
        cwd.join(relative),
    ]
    .into_iter()
    .find(|p| p.is_dir())
}
pub(crate) fn builtin_plugins(
    cwd: &Path,
    env: &Env,
) -> Result<Vec<Arc<crate::runtime::plugins::LoadedPlugin>>> {
    use crate::runtime::plugins::{
        PluginSource,
        loader::{discover_plugin_paths, load_plugin_from_path},
    };
    let Some(root) = resource_root(
        cwd,
        env,
        "PILOTDECK_BUILTIN_PLUGINS_DIR",
        "extension/plugins/builtin",
    ) else {
        return Ok(vec![]);
    };
    if !root.is_dir() {
        return Err(RouteError::new(
            "CONFIG_RESOURCE_NOT_FOUND",
            "Configured builtin plugin directory is unavailable",
        ));
    }
    discover_plugin_paths(&[(root, PluginSource::Builtin)])
        .into_iter()
        .map(|p| {
            load_plugin_from_path(&p.path, p.source)
                .map(Arc::new)
                .map_err(|e| RouteError::new("gateway_request_failed", e.message))
        })
        .collect()
}
struct BinarySpawner {
    executable: PathBuf,
    cwd: PathBuf,
    env: Env,
}
impl SidecarSpawner for BinarySpawner {
    fn requires_subprocess(&self) -> bool {
        true
    }

    fn command(&self) -> std::io::Result<tokio::process::Command> {
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .arg("--stdio")
            .current_dir(&self.cwd)
            .envs(&self.env);
        Ok(command)
    }
}
/// The T1 composition must retain this bootstrap.owner rather than instantiate
/// its private snapshot-only factory. Creation reads the current SDK/style state.
type Inputs = Arc<Mutex<std::collections::HashMap<JsString, Arc<SessionInput>>>>;
struct ConfigSessionFactory {
    inputs: Inputs,
    home: PathBuf,
    cwd: PathBuf,
    builtin_plugins: Vec<Arc<crate::runtime::plugins::LoadedPlugin>>,
    builtin_skills_root: Option<PathBuf>,
    spawner: Arc<dyn SidecarSpawner>,
}
impl SessionFactory for ConfigSessionFactory {
    fn can_recreate(&self) -> bool {
        true
    }
    fn create(
        &self,
        context: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<std::result::Result<LiveSessionHandle, RuntimeError>> {
        let inputs = self.inputs.clone();
        let home = self.home.clone();
        let cwd = self.cwd.clone();
        let builtin_plugins = self.builtin_plugins.clone();
        let builtin_skills_root = self.builtin_skills_root.clone();
        let spawner = self.spawner.clone();
        Box::pin(async move {
            if !inputs.lock().unwrap().contains_key(&context.session_key) {
                return Err(RuntimeError::new(
                    "Use ConfigBootstrap::session for native CONFIG turn admission",
                ));
            }
            let key = context.session_key.clone();
            let prepared = inputs
                .lock()
                .unwrap()
                .get(&key)
                .expect("prepared native session")
                .clone();
            let session_skills = prepared
                .sdk_config
                .as_ref()
                .and_then(|config| config.get("skills"))
                .and_then(skill_names);
            let project_root = context
                .project_key
                .as_ref()
                .and_then(|value| value.to_utf8().ok())
                .map(PathBuf::from)
                .unwrap_or_else(|| cwd.clone());
            let plugin_runtime = Arc::new(crate::runtime::plugins::PluginRuntime::new(
                crate::runtime::plugins::PluginRuntimeOptions {
                    project_root: project_root.clone(),
                    pilot_home: home,
                    builtin_skills_root,
                    builtin_plugins,
                    builtin_plugins_enabled: Default::default(),
                },
            ));
            plugin_runtime
                .refresh()
                .await
                .map_err(|error| RuntimeError::new(error.message))?;
            let contribution_lease = plugin_runtime
                .acquire_session_contributions(prepared.session_plugins.clone())
                .await
                .map_err(|error| RuntimeError::new(error.message))?;
            let resources = Arc::new(
                crate::runtime::session::context_resources::ContextResources::from_owned_runtime_lease(
                    plugin_runtime,
                    contribution_lease,
                    project_root.to_string_lossy().into_owned(),
                )
                .for_session_skills(session_skills.as_deref()),
            );
            let sidecar = SidecarSession::new(
                SidecarSessionConfig {
                    limits: crate::host::Limits::default(),
                    session_key: context.session_key,
                    project_key: context.project_key,
                    cwd: cwd.to_string_lossy().as_ref().into(),
                    services: None,
                    snapshot: prepared.snapshot.clone(),
                },
                spawner,
            )
            .with_owned_context_resources(resources);
            Ok(LiveSessionHandle::new(
                Arc::new(ConfigSession {
                    sidecar,
                    inputs,
                    key,
                }),
                None,
            ))
        })
    }
}

fn skill_names(value: &JsValue) -> Option<Vec<String>> {
    let JsValue::Array(values) = value else {
        return None;
    };
    Some(
        values
            .iter()
            .filter_map(JsValue::as_string)
            .map(|value| String::from_utf16_lossy(value.units()))
            .collect(),
    )
}
struct ConfigSession {
    sidecar: SidecarSession,
    inputs: Inputs,
    key: JsString,
}
impl LiveSession for ConfigSession {
    fn snapshot(&self) -> JsValue {
        let input = self
            .inputs
            .lock()
            .unwrap()
            .get(&self.key)
            .cloned()
            .expect("prepared native session");
        let mut value = self.sidecar.snapshot();
        let JsValue::Object(ref mut object) = value else {
            unreachable!("sidecar snapshot object");
        };
        object.insert(
            "configVersion".into(),
            super::field(&input.snapshot, "version").clone(),
        );
        object.insert(
            "thinking".into(),
            input.thinking.clone().unwrap_or(JsValue::Undefined),
        );
        if let Some(overrides) = &input.overrides {
            object.insert("overrides".into(), overrides.to_value());
        }
        value
    }
    fn submit(
        &self,
        input: JsValue,
        options: JsValue,
    ) -> std::result::Result<Box<dyn LiveTurn>, RuntimeError> {
        self.sidecar.submit(self.turn_input(input)?, options)
    }
    fn abort(&self, reason: &JsString) -> RuntimeResult {
        self.sidecar.abort(reason)
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        self.sidecar.dispose()
    }
}
impl ConfigSession {
    fn turn_input(&self, mut input: JsValue) -> std::result::Result<JsValue, RuntimeError> {
        let config = self
            .inputs
            .lock()
            .unwrap()
            .get(&self.key)
            .cloned()
            .expect("prepared native session");
        let JsValue::Object(ref mut object) = input else {
            return Err(RuntimeError::new("Turn input must be an object"));
        };
        let mut agent = super::field(super::field(&config.snapshot, "config"), "agent")
            .as_object()
            .cloned()
            .ok_or_else(|| RuntimeError::new("Native agent config is missing"))?;
        if let Some(settings) = config
            .sdk_config
            .as_ref()
            .and_then(|c| c.get("settings"))
            .and_then(|s| s.get("agent"))
            .and_then(JsValue::as_object)
        {
            for (k, v) in settings.entries() {
                agent.insert(k.clone(), v.clone());
            }
        }
        if let Some(model) = agent.get("model").and_then(JsValue::as_object).cloned() {
            if let Some(provider) = model.get("provider") {
                agent.insert("provider".into(), provider.clone());
            }
            if let Some(model) = model.get("model") {
                agent.insert("model".into(), model.clone());
            }
        }
        agent.insert(
            "cwd".into(),
            super::field(&self.sidecar.snapshot(), "cwd").clone(),
        );
        if let Some(thinking) = &config.thinking {
            agent.insert("thinking".into(), thinking.clone());
        }
        if let Some(content) = &config.output_style_content {
            let existing = agent
                .get("appendSystemPrompt")
                .and_then(JsValue::as_string)
                .map(|s| String::from_utf16_lossy(s.units()))
                .unwrap_or_default();
            let combined = if existing.is_empty() {
                content.clone()
            } else {
                format!("{existing}\n\n{content}")
            };
            agent.insert("appendSystemPrompt".into(), combined.as_str().into());
        }
        object.insert("agent".into(), JsValue::Object(agent.clone()));
        if let Some(mode) = config
            .overrides
            .as_ref()
            .and_then(|o| o.get("permissionMode"))
        {
            object.insert("basePermissionMode".into(), mode.to_value());
        }
        Ok(input)
    }
}

#[cfg(test)]
#[path = "bootstrap_tests.rs"]
mod tests;
