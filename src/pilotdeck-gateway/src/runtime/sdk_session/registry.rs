use super::{
    HandlerRegistration, ModelField, ModelRef, OrganizationPolicy, ResolveSettingsOptions,
    SdkConfigChangeHandler, SdkSessionError, SelectorKind, SessionHandlerRegistry,
    SessionHookHandler, apply_organization_task_budget_cap, array, assert_managed_model_allowed,
    assert_organization_setting_sources_allowed, field, is_null, is_organization_model_allowed,
    js_object, json_text, map_settings_error, nullish, number, number_text,
    organization_provider_policy_rejection, own, path_js, primitive_type, resolve_configured_model,
    resolve_session_settings, set_property, truthy, validate_managed_permissions,
    validate_selector_policy, validate_setting_sources,
};
use crate::{
    config::{Env, session_overrides::SessionConfigOverrides, session_value::SessionValue},
    js_value,
    runtime::plugins::{LoadedPlugin, PluginRuntime, PluginRuntimeError, PluginSkillContribution},
    storage::{TaskBudgetError, TaskBudgetStore},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};

// ─────────────────────────────────────────────────────────── runtime ports

/// One resolved project generation. The snapshot is the A6.3-pr2 shape; the
/// plugin runtime is the A6.3-pr7 owner.
pub struct ResolvedSdkRuntime {
    pub project_root: PathBuf,
    pub snapshot: JsValue,
    pub plugin_runtime: Arc<PluginRuntime>,
}

pub trait SdkRuntimeResolver: Send + Sync {
    fn resolve(
        &self,
        project_key: Option<&JsString>,
    ) -> Result<ResolvedSdkRuntime, SdkSessionError>;
}

pub type PluginLoadFuture<'a> =
    Pin<Box<dyn Future<Output = Result<LoadedPlugin, String>> + Send + 'a>>;

/// Path loader supplied by A6.3-pr7b. `resolveSdkSessionPlugins` owns the
/// realpath/duplicate/stat/name-conflict checks; only the manifest load is
/// delegated.
pub trait SdkPluginLoader: Send + Sync {
    fn load<'a>(&'a self, path: &'a Path) -> PluginLoadFuture<'a>;
}

/// Task-budget operations from A6.4-pr11. A6.3-pr8 decides *when* to call;
/// the store owns persistence and conflict text.
pub trait SdkTaskBudget: Send + Sync {
    fn configure_project(
        &self,
        project_root: &JsString,
        session_key: &JsString,
        total_usd: f64,
        retention_ms: Option<f64>,
        now_ms: f64,
    ) -> Result<(), TaskBudgetError>;
    fn clear_session(
        &self,
        project_root: &JsString,
        session_key: &JsString,
    ) -> Result<(), TaskBudgetError>;
}

/// Adapter over the merged [`TaskBudgetStore`].
pub struct TaskBudgetStorePort {
    store: Mutex<TaskBudgetStore>,
}

impl TaskBudgetStorePort {
    pub fn new(store: TaskBudgetStore) -> Self {
        Self {
            store: Mutex::new(store),
        }
    }
}

impl SdkTaskBudget for TaskBudgetStorePort {
    fn configure_project(
        &self,
        project_root: &JsString,
        session_key: &JsString,
        total_usd: f64,
        retention_ms: Option<f64>,
        now_ms: f64,
    ) -> Result<(), TaskBudgetError> {
        self.store
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .configure_project(project_root, session_key, total_usd, retention_ms, now_ms)
    }
    fn clear_session(
        &self,
        project_root: &JsString,
        session_key: &JsString,
    ) -> Result<(), TaskBudgetError> {
        self.store
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear_session(project_root, session_key)
    }
}

/// The subset of `AbortSignal` used by `setSdkSessionConfig`.
pub trait SdkAbortSignal {
    fn throw_if_aborted(&self) -> Result<(), SdkSessionError>;
}

fn check_abort(signal: Option<&dyn SdkAbortSignal>) -> Result<(), SdkSessionError> {
    match signal {
        Some(signal) => signal.throw_if_aborted(),
        None => Ok(()),
    }
}

// ─────────────────────────────────────────────────────────── registry

pub struct SdkSessionOptions {
    pub fallback_project_root: PathBuf,
    pub cwd: PathBuf,
    pub env: Env,
    /// `os.homedir()` override; `None` reads the process HOME like Node.
    pub homedir: Option<PathBuf>,
    pub resolver: Arc<dyn SdkRuntimeResolver>,
    pub organization_policy: Option<OrganizationPolicy>,
    pub plugin_loader: Option<Arc<dyn SdkPluginLoader>>,
    pub task_budget: Option<Arc<dyn SdkTaskBudget>>,
    pub clock: Arc<dyn Fn() -> f64 + Send + Sync>,
}

impl SdkSessionOptions {
    pub fn new(
        fallback_project_root: impl Into<PathBuf>,
        cwd: impl Into<PathBuf>,
        env: Env,
        resolver: Arc<dyn SdkRuntimeResolver>,
    ) -> Self {
        Self {
            fallback_project_root: fallback_project_root.into(),
            cwd: cwd.into(),
            env,
            homedir: None,
            resolver,
            organization_policy: None,
            plugin_loader: None,
            task_budget: None,
            clock: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis() as f64)
                    .unwrap_or(0.0)
            }),
        }
    }
    pub fn with_organization_policy(mut self, policy: Option<OrganizationPolicy>) -> Self {
        self.organization_policy = policy;
        self
    }
    pub fn with_homedir(mut self, homedir: impl Into<PathBuf>) -> Self {
        self.homedir = Some(homedir.into());
        self
    }
    pub fn with_plugin_loader(mut self, loader: Arc<dyn SdkPluginLoader>) -> Self {
        self.plugin_loader = Some(loader);
        self
    }
    pub fn with_task_budget(mut self, budget: Arc<dyn SdkTaskBudget>) -> Self {
        self.task_budget = Some(budget);
        self
    }
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> f64 + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlagSettingsResult {
    pub applied: Vec<&'static str>,
    pub cleared: Vec<&'static str>,
}

pub struct SdkSessionRegistry {
    options: SdkSessionOptions,
    configs: Vec<(JsString, JsValue)>,
    plugins: Vec<(JsString, Vec<Arc<LoadedPlugin>>)>,
    thinking: Vec<(JsString, JsValue)>,
    projects: Vec<(JsString, PathBuf)>,
    overrides: Option<SessionConfigOverrides>,
    handlers: SessionHandlerRegistry,
}

impl SdkSessionRegistry {
    pub fn new(options: SdkSessionOptions) -> Self {
        Self {
            options,
            configs: Vec::new(),
            plugins: Vec::new(),
            thinking: Vec::new(),
            projects: Vec::new(),
            overrides: None,
            handlers: SessionHandlerRegistry::new(),
        }
    }

    pub fn with_overrides(mut self, overrides: SessionConfigOverrides) -> Self {
        self.overrides = Some(overrides);
        self
    }

    pub fn options(&self) -> &SdkSessionOptions {
        &self.options
    }

    pub fn handlers(&self) -> &SessionHandlerRegistry {
        &self.handlers
    }

    pub fn handlers_mut(&mut self) -> &mut SessionHandlerRegistry {
        &mut self.handlers
    }

    /// Full configuration insertion order, for fixture replay and inspection.
    pub fn config_entries(&self) -> &[(JsString, JsValue)] {
        &self.configs
    }
    pub fn plugin_entries(&self) -> &[(JsString, Vec<Arc<LoadedPlugin>>)] {
        &self.plugins
    }
    pub fn thinking_entries(&self) -> &[(JsString, JsValue)] {
        &self.thinking
    }
    pub fn project_entries(&self) -> &[(JsString, PathBuf)] {
        &self.projects
    }

    pub fn session_config(&self, session_key: &JsString) -> Option<&JsValue> {
        self.configs
            .iter()
            .find(|(key, _)| key == session_key)
            .map(|(_, value)| value)
    }

    pub fn session_plugins(&self, session_key: &JsString) -> &[Arc<LoadedPlugin>] {
        self.plugins
            .iter()
            .find(|(key, _)| key == session_key)
            .map(|(_, plugins)| plugins.as_slice())
            .unwrap_or(&[])
    }

    pub fn session_project(&self, session_key: &JsString) -> Option<&Path> {
        self.projects
            .iter()
            .find(|(key, _)| key == session_key)
            .map(|(_, root)| root.as_path())
    }

    /// `None` means no override entry; `Some(Null)` is an explicit `null`.
    pub fn thinking_override(&self, session_key: &JsString) -> Option<&JsValue> {
        self.thinking
            .iter()
            .find(|(key, _)| key == session_key)
            .map(|(_, value)| value)
    }

    pub fn session_overrides(&self) -> Option<&SessionConfigOverrides> {
        self.overrides.as_ref()
    }

    fn set_config(&mut self, session_key: &JsString, config: JsValue) {
        match self.configs.iter_mut().find(|(key, _)| key == session_key) {
            Some((_, value)) => *value = config,
            None => self.configs.push((session_key.clone(), config)),
        }
    }

    fn delete_config(&mut self, session_key: &JsString) {
        self.configs.retain(|(key, _)| key != session_key);
    }

    fn set_plugins(&mut self, session_key: &JsString, plugins: Vec<Arc<LoadedPlugin>>) {
        match self.plugins.iter_mut().find(|(key, _)| key == session_key) {
            Some((_, value)) => *value = plugins,
            None => self.plugins.push((session_key.clone(), plugins)),
        }
    }

    fn delete_plugins(&mut self, session_key: &JsString) {
        self.plugins.retain(|(key, _)| key != session_key);
    }

    fn set_thinking(&mut self, session_key: &JsString, value: JsValue) {
        match self.thinking.iter_mut().find(|(key, _)| key == session_key) {
            Some((_, current)) => *current = value,
            None => self.thinking.push((session_key.clone(), value)),
        }
    }

    fn delete_thinking(&mut self, session_key: &JsString) {
        self.thinking.retain(|(key, _)| key != session_key);
    }

    fn set_project(&mut self, session_key: &JsString, root: PathBuf) {
        match self.projects.iter_mut().find(|(key, _)| key == session_key) {
            Some((_, current)) => *current = root,
            None => self.projects.push((session_key.clone(), root)),
        }
    }

    fn delete_project(&mut self, session_key: &JsString) {
        self.projects.retain(|(key, _)| key != session_key);
    }

    fn resolve_settings(
        &self,
        runtime: &ResolvedSdkRuntime,
        config: &JsValue,
    ) -> Result<Option<JsValue>, SdkSessionError> {
        let settings = self
            .options
            .organization_policy
            .as_ref()
            .and_then(|policy| policy.settings.as_ref());
        resolve_session_settings(ResolveSettingsOptions {
            project_root: &runtime.project_root,
            cwd: &self.options.cwd,
            env: &self.options.env,
            homedir: self.options.homedir.as_deref(),
            host_defaults: settings.and_then(|value| value.session_defaults.as_ref()),
            host_managed_settings: settings
                .and_then(|value| value.managed_session_settings.as_ref()),
            host_setting_sources: settings.and_then(|value| value.session_default_sources.as_ref()),
            settings: own(config, "settings"),
            setting_sources: own(config, "settingSources"),
            host_enforced_settings: settings
                .and_then(|value| value.enforced_session_settings.as_ref()),
        })
        .map_err(map_settings_error)
    }

    fn assert_organization_model_allowed(
        &self,
        runtime: &ResolvedSdkRuntime,
        model: &ModelRef,
    ) -> Result<(), SdkSessionError> {
        let policy = self.options.organization_policy.as_ref();
        if !is_organization_model_allowed(policy.and_then(|policy| policy.models.as_ref()), model) {
            return Err(SdkSessionError::new(
                "GATEWAY_ORGANIZATION_MODEL_DENIED",
                format!(
                    "Gateway organization policy denies model {}/{}.",
                    number_text(&model.provider),
                    number_text(&model.model)
                ),
            ));
        }
        let provider = field(field(&runtime.snapshot, "config"), "model")
            .get("providers")
            .and_then(|providers| providers.get(&number_text(&model.provider)));
        if let Some(rejection) = organization_provider_policy_rejection(
            policy.and_then(|policy| policy.providers.as_ref()),
            &model.provider,
            provider,
        ) {
            return Err(rejection);
        }
        Ok(())
    }

    /// `resolveSdkSessionConfig`; the settings key is always present (as
    /// `undefined` when no overlay resolves), matching the TS object spread.
    pub fn resolve_session_config(
        &self,
        session_key: &JsString,
        runtime: &ResolvedSdkRuntime,
    ) -> Result<Option<JsValue>, SdkSessionError> {
        let Some(stored) = self.session_config(session_key) else {
            return Ok(None);
        };
        let settings = self.resolve_settings(runtime, stored)?;
        let mut resolved = stored.clone();
        set_property(
            &mut resolved,
            "settings",
            settings.unwrap_or(JsValue::Undefined),
        );
        Ok(Some(resolved))
    }

    /// `assertSdkModelAllowed`.
    pub fn assert_model_allowed(
        &self,
        session_key: &JsString,
        model: Option<&ModelRef>,
        project_key: Option<&JsString>,
    ) -> Result<(), SdkSessionError> {
        let project_root = match project_key {
            Some(key) => key.clone(),
            None => match self.session_project(session_key) {
                Some(root) => path_js(root),
                None => path_js(&self.options.fallback_project_root),
            },
        };
        let runtime = self.options.resolver.resolve(Some(&project_root))?;
        let resolved = match model {
            Some(model) => model.clone(),
            None => {
                let requested = self
                    .resolve_session_config(session_key, &runtime)?
                    .and_then(|config| {
                        field(&config, "settings")
                            .get("agent")
                            .and_then(|agent| agent.get("model"))
                            .cloned()
                    });
                match requested {
                    Some(value) if truthy(&value) => {
                        let Some(requested) = value.as_string() else {
                            return Err(SdkSessionError::new(
                                "INVALID_SDK_AGENT_MODEL",
                                format!(
                                    "settings.agent.model is not uniquely resolvable from the Gateway catalog: {}",
                                    number_text(&JsString::from(String::from_utf16_lossy(
                                        js_value::to_string_units(&value)
                                            .unwrap_or_default()
                                            .as_slice()
                                    )))
                                ),
                            ));
                        };
                        resolve_configured_model(
                            &runtime.snapshot,
                            requested,
                            ModelField::AgentModel,
                        )?
                    }
                    _ => {
                        let configured =
                            field(field(&runtime.snapshot, "config"), "agent").get("model");
                        let provider = configured
                            .and_then(|model| model.get("provider"))
                            .and_then(JsValue::as_string)
                            .cloned()
                            .unwrap_or_default();
                        let name = configured
                            .and_then(|model| model.get("model"))
                            .and_then(JsValue::as_string)
                            .cloned()
                            .unwrap_or_default();
                        ModelRef {
                            provider,
                            model: name,
                        }
                    }
                }
            }
        };
        self.assert_organization_model_allowed(&runtime, &resolved)?;
        assert_managed_model_allowed(
            self.session_config(session_key)
                .and_then(|config| own(config, "managedModels")),
            &resolved,
        )
    }

    /// `setSessionThinkingForSdk`: `None` deletes the override, `Some(Null)`
    /// stores an explicit `null`, and objects are cloned.
    pub fn set_session_thinking(&mut self, session_key: &JsString, thinking: Option<&JsValue>) {
        match thinking {
            None => self.delete_thinking(session_key),
            Some(value) => self.set_thinking(session_key, value.clone()),
        }
    }

    /// `applyFlagSettingsForSdk`.
    pub fn apply_flag_settings(
        &mut self,
        session_key: &JsString,
        settings: &JsValue,
    ) -> Result<FlagSettingsResult, SdkSessionError> {
        let mut result = FlagSettingsResult::default();
        let has = |key: &str| {
            settings
                .as_object()
                .is_some_and(|object| object.contains_key(key))
        };
        if has("effortLevel") {
            let effort = field(settings, "effortLevel");
            if is_null(effort) {
                self.delete_thinking(session_key);
                result.cleared.push("effortLevel");
            } else {
                self.set_thinking(
                    session_key,
                    js_object(vec![
                        ("enabled", JsValue::Bool(true)),
                        ("mode", effort.clone()),
                    ]),
                );
                result.applied.push("effortLevel");
            }
        }
        if has("permissions") {
            let permissions = field(settings, "permissions");
            if permissions.is_undefined() {
                return Err(SdkSessionError::raw(
                    "TypeError",
                    "Cannot read properties of undefined (reading 'defaultMode')",
                ));
            }
            let default_mode = if is_null(permissions) {
                JsValue::Null
            } else {
                field(permissions, "defaultMode").clone()
            };
            if is_null(&default_mode) || default_mode.is_undefined() {
                if let Some(overrides) = &mut self.overrides
                    && let Some(existing) = overrides.get(session_key)
                    && existing
                        .get("permissionMode")
                        .is_some_and(|value| !value.to_value().is_undefined())
                {
                    // TS `const { permissionMode: _removed, ...rest } = existing`
                    // replaces the top level only; nested handles stay shared.
                    let next = existing.spread();
                    let _removed = next.remove("permissionMode");
                    overrides.set(session_key, &next);
                }
                result.cleared.push("permissions.defaultMode");
            } else {
                if let Some(overrides) = &mut self.overrides {
                    // TS `{ ...existing, permissionMode }`: `get` already
                    // returns a fresh top level sharing the nested nodes.
                    let existing = overrides
                        .get(session_key)
                        .unwrap_or_else(SessionValue::object);
                    existing.set("permissionMode", default_mode);
                    overrides.set(session_key, &existing);
                }
                result.applied.push("permissions.defaultMode");
            }
        }
        Ok(result)
    }

    pub fn register_config_change_handler(
        &mut self,
        session_key: &JsString,
        handler: SdkConfigChangeHandler,
    ) -> HandlerRegistration {
        self.handlers.register_config_change(session_key, handler)
    }

    pub fn register_session_hook_handler(
        &mut self,
        session_key: &JsString,
        handler: SessionHookHandler,
    ) -> HandlerRegistration {
        self.handlers.register_session_hook(session_key, handler)
    }

    pub fn is_ephemeral_session(&self, session_key: &JsString) -> bool {
        self.session_config(session_key)
            .and_then(|config| own(config, "persistSession"))
            .is_some_and(|value| matches!(value, JsValue::Bool(false)))
    }

    /// `clearSdkSessionState`. MCP fields are owned by A6.5 and not modeled
    /// here; the task-budget clear is called only when a project was recorded.
    pub fn clear_session_state(&mut self, session_key: &JsString) -> Result<(), SdkSessionError> {
        if let Some(root) = self.session_project(session_key).map(Path::to_path_buf)
            && let Some(budget) = &self.options.task_budget
        {
            budget
                .clear_session(&path_js(&root), session_key)
                .map_err(task_budget_error)?;
        }
        self.delete_config(session_key);
        self.delete_plugins(session_key);
        self.delete_thinking(session_key);
        self.delete_project(session_key);
        self.handlers.clear_session(session_key);
        Ok(())
    }

    async fn resolve_session_plugins(
        &self,
        requested: Option<&JsValue>,
        runtime: &ResolvedSdkRuntime,
    ) -> Result<Vec<Arc<LoadedPlugin>>, SdkSessionError> {
        let Some(requested) = requested else {
            return Ok(Vec::new());
        };
        let Some(descriptors) = array(requested) else {
            return Ok(Vec::new());
        };
        if descriptors.is_empty() {
            return Ok(Vec::new());
        }
        let mut names: Vec<String> = runtime
            .plugin_runtime
            .snapshot()
            .await
            .iter()
            .map(|plugin| plugin.name.clone())
            .collect();
        let mut paths: Vec<PathBuf> = Vec::new();
        let mut plugins = Vec::new();
        for descriptor in descriptors {
            let requested_path = field(descriptor, "path");
            let Some(requested_text) = requested_path.as_string() else {
                return Err(SdkSessionError::new(
                    "SDK_PLUGIN_NOT_FOUND",
                    format!(
                        "Gateway-local SDK plugin path does not exist: {}",
                        display_value(requested_path)
                    ),
                ));
            };
            let canonical = std::fs::canonicalize(Path::new(&number_text(requested_text)))
                .map_err(|_| {
                    SdkSessionError::new(
                        "SDK_PLUGIN_NOT_FOUND",
                        format!(
                            "Gateway-local SDK plugin path does not exist: {}",
                            number_text(requested_text)
                        ),
                    )
                })?;
            if paths.contains(&canonical) {
                return Err(SdkSessionError::new(
                    "SDK_PLUGIN_DUPLICATE_PATH",
                    format!(
                        "Gateway-local SDK plugin path is listed more than once: {}",
                        number_text(requested_text)
                    ),
                ));
            }
            paths.push(canonical.clone());
            let metadata = std::fs::metadata(&canonical).map_err(|_| {
                SdkSessionError::new(
                    "SDK_PLUGIN_NOT_FOUND",
                    format!(
                        "Gateway-local SDK plugin path does not exist: {}",
                        number_text(requested_text)
                    ),
                )
            })?;
            if !metadata.is_dir() {
                return Err(SdkSessionError::new(
                    "SDK_PLUGIN_NOT_DIRECTORY",
                    format!(
                        "Gateway-local SDK plugin path is not a directory: {}",
                        number_text(requested_text)
                    ),
                ));
            }
            let Some(loader) = &self.options.plugin_loader else {
                return Err(SdkSessionError::new(
                    "SDK_PLUGIN_LOAD_FAILED",
                    format!(
                        "Could not load SDK plugin {}: plugin loading is not available.",
                        number_text(requested_text)
                    ),
                ));
            };
            let plugin = loader.load(&canonical).await.map_err(|error| {
                SdkSessionError::new(
                    "SDK_PLUGIN_LOAD_FAILED",
                    format!(
                        "Could not load SDK plugin {}: {error}",
                        number_text(requested_text)
                    ),
                )
            })?;
            if names.iter().any(|name| name == &plugin.name) {
                return Err(SdkSessionError::new(
                    "SDK_PLUGIN_NAME_CONFLICT",
                    format!(
                        "SDK plugin {} conflicts with an already discovered project plugin.",
                        plugin.name
                    ),
                ));
            }
            names.push(plugin.name.clone());
            plugins.push(Arc::new(plugin));
        }
        Ok(plugins)
    }

    fn configure_project_task_budget(
        &self,
        project_root: &Path,
        session_key: &JsString,
        task_budget: Option<&JsValue>,
        signal: Option<&dyn SdkAbortSignal>,
    ) -> Result<(), SdkSessionError> {
        let Some(task_budget) = task_budget else {
            return Ok(());
        };
        if !field(task_budget, "scope")
            .as_string()
            .is_some_and(|scope| scope.equals_str("project"))
        {
            return Ok(());
        }
        let Some(budget) = &self.options.task_budget else {
            return Err(SdkSessionError::new(
                "gateway_request_failed",
                "SDK task budget storage is not configured.",
            ));
        };
        check_abort(signal)?;
        let total = number(field(task_budget, "total"));
        let retention = own(task_budget, "projectRetentionMs")
            .filter(|value| !value.is_undefined())
            .map(number);
        budget
            .configure_project(
                &path_js(project_root),
                session_key,
                total,
                retention,
                (self.options.clock)(),
            )
            .map_err(task_budget_error)
    }

    /// `setSdkSessionConfig`. Validation and every fail-fast check run before
    /// any state mutation, exactly as in the baseline.
    pub async fn set_session_config(
        &mut self,
        session_key: &JsString,
        config: &JsValue,
        project_key: Option<&JsString>,
        signal: Option<&dyn SdkAbortSignal>,
    ) -> Result<bool, SdkSessionError> {
        check_abort(signal)?;
        let cap = self
            .options
            .organization_policy
            .as_ref()
            .and_then(|policy| policy.max_task_budget_usd);
        let mut next = config.clone();
        // The baseline reads `next.taskBudget`/`next.managedPermissions`; null
        // and undefined throw there before any validation or state change.
        if nullish(&next) {
            let field = if cap.is_some() {
                "taskBudget"
            } else {
                "managedPermissions"
            };
            return Err(SdkSessionError::raw(
                "TypeError",
                format!(
                    "Cannot read properties of {} (reading '{field}')",
                    if is_null(&next) { "null" } else { "undefined" }
                ),
            ));
        }
        // A defined cap assigns `next.taskBudget`; sloppy-mode CJS would
        // silently ignore that on a primitive, but the ES-module baseline is
        // strict and throws. Arrays accept the (invisible) property, so only
        // numbers, strings, and booleans reject.
        if cap.is_some()
            && let Some(kind) = primitive_type(&next)
        {
            return Err(SdkSessionError::raw(
                "TypeError",
                format!(
                    "Cannot create property 'taskBudget' on {kind} '{}'",
                    js_value::display(&next).unwrap_or_default()
                ),
            ));
        }
        apply_organization_task_budget_cap(&mut next, cap);
        validate_managed_permissions(own(&next, "managedPermissions"))?;
        validate_selector_policy(own(&next, "managedTools"), SelectorKind::Tools)?;
        validate_selector_policy(own(&next, "managedModels"), SelectorKind::Models)?;
        validate_setting_sources(own(&next, "settingSources")).map_err(map_settings_error)?;
        assert_organization_setting_sources_allowed(
            self.options
                .organization_policy
                .as_ref()
                .and_then(|policy| policy.setting_sources.as_ref()),
            own(&next, "settingSources"),
        )?;
        let previous = self.session_config(session_key).cloned();
        let runtime = self.options.resolver.resolve(project_key)?;
        let resolved_settings = self.resolve_settings(&runtime, &next)?;
        for (requested, field) in [
            (
                resolved_settings
                    .as_ref()
                    .and_then(|settings| settings.get("agent"))
                    .and_then(|agent| agent.get("model")),
                ModelField::AgentModel,
            ),
            (
                resolved_settings
                    .as_ref()
                    .and_then(|settings| settings.get("agent"))
                    .and_then(|agent| agent.get("fallbackModel")),
                ModelField::FallbackModel,
            ),
            (
                resolved_settings
                    .as_ref()
                    .and_then(|settings| settings.get("agent"))
                    .and_then(|agent| agent.get("subagents"))
                    .and_then(|subagents| subagents.get("default")),
                ModelField::SubagentModel,
            ),
        ] {
            let Some(requested) = requested else {
                continue;
            };
            if nullish(requested) {
                continue;
            }
            let model = resolve_configured_model_value(&runtime.snapshot, requested, field)?;
            self.assert_organization_model_allowed(&runtime, &model)?;
            assert_managed_model_allowed(own(&next, "managedModels"), &model)?;
        }

        let has_host_defaults = self
            .options
            .organization_policy
            .as_ref()
            .is_some_and(OrganizationPolicy::has_session_defaults);
        let same_json = match &previous {
            Some(previous) => json_text(previous) == json_text(&next),
            None => json_text(&next).is_none(),
        };
        if same_json && !(has_host_defaults && previous.is_none()) {
            return Ok(false);
        }

        runtime
            .plugin_runtime
            .refresh()
            .await
            .map_err(plugin_error)?;
        check_abort(signal)?;
        let session_plugins = self
            .resolve_session_plugins(own(&next, "plugins"), &runtime)
            .await?;
        let view = runtime
            .plugin_runtime
            .create_view(session_plugins.clone())
            .await;
        if let Some(skills) =
            own(&next, "skills").filter(|value| matches!(value, JsValue::Array(_)))
        {
            let available = view.all_skills();
            let items = array(skills).unwrap_or(&[]);
            let resolved = resolve_session_skills(items, &available)?;
            set_property(&mut next, "skills", resolved);
        }
        // resolveSdkSubagentScopes (TS :2118) is outside this slice's ranges.

        if let Some(model) = own(&next, "settings")
            .and_then(|settings| settings.get("agent"))
            .and_then(|agent| agent.get("model"))
            .filter(|value| truthy(value))
        {
            let resolved =
                resolve_configured_model_value(&runtime.snapshot, model, ModelField::Model)?;
            self.assert_organization_model_allowed(&runtime, &resolved)?;
            assert_managed_model_allowed(own(&next, "managedModels"), &resolved)?;
        }
        if let Some(fallback) = own(&next, "fallbackModel").filter(|value| truthy(value)) {
            let resolved = resolve_configured_model_value(
                &runtime.snapshot,
                fallback,
                ModelField::FallbackModel,
            )?;
            self.assert_organization_model_allowed(&runtime, &resolved)?;
            assert_managed_model_allowed(own(&next, "managedModels"), &resolved)?;
        }

        self.configure_project_task_budget(
            &runtime.project_root,
            session_key,
            own(&next, "taskBudget"),
            signal,
        )?;
        check_abort(signal)?;
        self.set_config(session_key, next.clone());
        if session_plugins.is_empty() {
            self.delete_plugins(session_key);
        } else {
            self.set_plugins(session_key, session_plugins);
        }
        self.set_project(session_key, runtime.project_root);
        Ok(true)
    }
}

fn resolve_configured_model_value(
    snapshot: &JsValue,
    requested: &JsValue,
    model_field: ModelField,
) -> Result<ModelRef, SdkSessionError> {
    match requested.as_string() {
        Some(requested) => resolve_configured_model(snapshot, requested, model_field),
        None => Err(SdkSessionError::new(
            model_field.code(),
            format!(
                "{} is not uniquely resolvable from the Gateway catalog: {}",
                model_field.label(),
                display_value(requested)
            ),
        )),
    }
}

fn display_value(value: &JsValue) -> String {
    js_value::display(value).unwrap_or_else(|_| "Maximum call stack size exceeded".into())
}

pub fn resolve_session_skills(
    requested: &[JsValue],
    available: &[PluginSkillContribution],
) -> Result<JsValue, SdkSessionError> {
    let mut resolved: Vec<String> = Vec::new();
    for requested_name in requested {
        let requested_text = display_value(requested_name);
        let exact: Vec<&PluginSkillContribution> = available
            .iter()
            .filter(|skill| skill.name == requested_text)
            .collect();
        let candidates: Vec<&PluginSkillContribution> = if exact.is_empty() {
            let suffix = format!(":{requested_text}");
            available
                .iter()
                .filter(|skill| skill.name.ends_with(&suffix))
                .collect()
        } else {
            exact
        };
        if candidates.is_empty() {
            return Err(SdkSessionError::new(
                "SDK_SKILL_NOT_FOUND",
                format!("SDK session skill is not available in this project: {requested_text}"),
            ));
        }
        if candidates.len() > 1 {
            return Err(SdkSessionError::new(
                "SDK_SKILL_AMBIGUOUS",
                format!(
                    "SDK session skill is ambiguous; use its fully-qualified name: {requested_text}"
                ),
            ));
        }
        let name = candidates[0].name.clone();
        if !resolved.contains(&name) {
            resolved.push(name);
        }
    }
    Ok(JsValue::Array(
        resolved
            .into_iter()
            .map(|name| JsValue::String(JsString::from(name)))
            .collect(),
    ))
}

fn task_budget_error(error: TaskBudgetError) -> SdkSessionError {
    match error.code() {
        Some(code) => SdkSessionError::new(code, error.to_string()),
        None => SdkSessionError::raw("Error", error.to_string()),
    }
}

fn plugin_error(error: PluginRuntimeError) -> SdkSessionError {
    SdkSessionError::raw("Error", error.to_string())
}
