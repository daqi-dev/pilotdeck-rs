//! Native CONFIG wire adapter. State is shared across connections, never rebuilt per RPC.

use pilotdeck_protocol::{JsObject, JsValue};
use std::{future::Future, pin::Pin};

pub type ChannelCall<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
pub type Result<T = JsValue> = std::result::Result<T, RouteError>;

#[derive(Clone, Debug)]
pub struct RouteError {
    pub code: String,
    pub message: String,
    /// Retain UTF-16, provider details and validation at the ordered boundary.
    pub ordered_error: Option<Box<JsValue>>,
}
impl RouteError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), ordered_error: None }
    }
    pub fn from_ordered(error: JsValue) -> Self {
        let text = |key| error.get(key).and_then(JsValue::as_string)
            .map(|s| String::from_utf16_lossy(s.units()));
        Self {
            code: text("code").unwrap_or_else(|| "gateway_request_failed".into()),
            message: text("message").unwrap_or_else(|| "Native channel request failed.".into()),
            ordered_error: Some(Box::new(error)),
        }
    }
    pub fn to_js(&self) -> JsValue {
        if let Some(error) = &self.ordered_error { return (**error).clone(); }
        let mut error = JsObject::new();
        error.insert("code".into(), self.code.as_str().into());
        error.insert("message".into(), self.message.as_str().into());
        JsValue::Object(error)
    }
}
impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for RouteError {}
impl From<crate::runtime::connection::RequestError> for RouteError {
    fn from(error: crate::runtime::connection::RequestError) -> Self {
        let mut value = JsObject::new();
        value.insert("code".into(), error.code.as_str().into());
        value.insert("message".into(), JsValue::String(error.message));
        if let Some(v) = error.validation { value.insert("validation".into(), *v); }
        else if let Some(v) = error.details { value.insert("details".into(), *v); }
        Self::from_ordered(JsValue::Object(value))
    }
}
impl From<&super::Error> for RouteError {
    fn from(error: &super::Error) -> Self {
        Self::new(&error.code, String::from_utf16_lossy(error.message.units()))
    }
}

use super::{
    Env,
    store::{ConfigStore, StoreOptions},
};
use crate::metadata::{object, text_field};
use pilotdeck_protocol::JsString;
use std::path::PathBuf;

pub mod bluebubbles;
pub mod bootstrap;
pub mod channel_http;
mod commands;
pub mod channel_inbound;
mod channel_commands;
pub mod dingtalk;
pub mod discord;
pub mod email;
pub mod feishu;
pub mod home_assistant;
pub mod im_cron;
pub mod channel_state;
pub mod channel_webhook;
pub use channel_state::ChannelStatePersistence;
mod local_settings;
pub mod matrix;
pub mod mattermost;
pub mod native;
pub mod qq;
mod session;
pub mod wecom;
pub mod wecom_callback;
pub use session::SessionInput;
pub mod signal;
pub mod slack;
pub mod sms;
mod styles;
pub mod telegram;
pub(crate) mod upstream_turns;
mod weixin;
pub mod whatsapp;
pub use weixin::{
    NativeWeixinFactory, WeixinChannel, WeixinChannelFactory, WeixinChannelHost,
    WeixinDelivery, WeixinInbox,
};

pub const METHODS: &[&str] = &[
    "update_settings",
    "resolve_settings",
    "apply_flag_settings",
    "set_permission_mode",
    "set_session_thinking",
    "output_styles_list",
    "set_output_style",
    "reload_output_styles",
    "commands_list",
    "reload_config",
    "reload_extensions",
    "prepare_weixin_login",
    "dispatch_im_inbound_slash_command",
];
pub(super) fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
pub(super) fn strings(values: impl IntoIterator<Item = impl Into<JsString>>) -> JsValue {
    JsValue::Array(
        values
            .into_iter()
            .map(|v| JsValue::String(v.into()))
            .collect(),
    )
}

pub struct ConfigRoutes {
    pub store: ConfigStore,
    resolver: std::sync::Arc<session::Resolver>,
    output_styles: std::collections::HashMap<
        PathBuf,
        Vec<crate::runtime::plugins::PluginOutputStyleContribution>,
    >,
    pub sdk: crate::runtime::sdk_session::SdkSessionRegistry,
    pub(crate) shared_sdk:
        Option<std::sync::Arc<tokio::sync::Mutex<crate::runtime::sdk_session::SdkSessionRegistry>>>,
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub env: Env,
    pub session_owner: Option<crate::runtime::session::SessionOwner>,
    pub permission_modes:
        std::sync::Arc<std::sync::Mutex<super::permission::GatewaySessionPermissionModeRegistry>>,
    pub interactions:
        Option<std::sync::Arc<crate::runtime::interaction::GatewayInteractionCoordinator>>,
    pub dialog_project_keys:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<JsString, JsString>>>,
    organization_policy: OrganizationPolicy,
    host_bound: bool,
    stopped: bool,
    pub weixin_login: Option<std::sync::Arc<dyn WeixinLoginHost>>,
    pub memory_scheduler: Option<std::sync::Arc<crate::runtime::memory::WebMemoryScheduler>>,
    notices: tokio::sync::broadcast::Sender<JsValue>,
    refresh_tasks: tokio::task::JoinSet<()>,
    watcher: Option<super::store::WatchHandle>,
    watch_subscription: Option<super::store::Subscription>,
}
impl ConfigRoutes {
    pub fn new(home: PathBuf, cwd: PathBuf, env: Env) -> Result<Self> {
        Self::new_with_plugins(home, cwd, env, vec![])
    }
    pub fn new_with_plugins(
        home: PathBuf,
        cwd: PathBuf,
        env: Env,
        builtin_plugins: Vec<std::sync::Arc<crate::runtime::plugins::LoadedPlugin>>,
    ) -> Result<Self> {
        Self::new_with_resources(home, cwd, env, builtin_plugins, None)
    }
    pub fn new_with_resources(
        home: PathBuf,
        cwd: PathBuf,
        mut env: Env,
        builtin_plugins: Vec<std::sync::Arc<crate::runtime::plugins::LoadedPlugin>>,
        builtin_skills_root: Option<PathBuf>,
    ) -> Result<Self> {
        env.insert("PILOT_HOME".into(), home.to_string_lossy().into_owned());
        let mut options = StoreOptions::new(&home, &cwd, env.clone());
        options.project = Some(cwd.to_string_lossy().into_owned());
        let store = ConfigStore::new(options).map_err(|e| RouteError::from(&e))?;
        let resolver = std::sync::Arc::new(session::Resolver::new(
            home.clone(),
            cwd.clone(),
            env.clone(),
            store.clone(),
            builtin_plugins,
            builtin_skills_root,
        ));
        let sdk = crate::runtime::sdk_session::SdkSessionRegistry::new(
            crate::runtime::sdk_session::SdkSessionOptions::new(
                &cwd,
                &cwd,
                env.clone(),
                resolver.clone(),
            ),
        )
        .with_overrides(super::session_overrides::SessionConfigOverrides::new());
        Ok(Self {
            resolver,
            output_styles: Default::default(),
            sdk,
            shared_sdk: None,
            store,
            home,
            cwd,
            env,
            session_owner: None,
            permission_modes: Default::default(),
            interactions: None,
            dialog_project_keys: Default::default(),
            organization_policy: OrganizationPolicy::default(),
            host_bound: false,
            stopped: false,
            weixin_login: None,
            memory_scheduler: None,
            notices: tokio::sync::broadcast::channel(256).0,
            refresh_tasks: tokio::task::JoinSet::new(),
            watcher: None,
            watch_subscription: None,
        })
    }
    pub async fn handle(&mut self, method: &str, params: &JsValue) -> Option<Result> {
        let shared = self.shared_sdk.clone();
        let mut sdk = match &shared {
            Some(shared) => Some(shared.lock().await),
            None => None,
        };
        if let Some(sdk) = &mut sdk {
            std::mem::swap(&mut self.sdk, sdk);
        }
        let result = self.handle_owned(method, params).await;
        if let Some(sdk) = &mut sdk {
            std::mem::swap(&mut self.sdk, sdk);
        }
        result
    }
    async fn handle_owned(&mut self, method: &str, params: &JsValue) -> Option<Result> {
        if self.stopped {
            return Some(Err(RouteError::new(
                "CONFIG_HOST_STOPPED",
                "Native CONFIG host stopped",
            )));
        }
        while self.refresh_tasks.try_join_next().is_some() {}
        Some(match method {
            "update_settings" => self.update_settings(params).await,
            "resolve_settings" => Ok(self.resolve_settings()),
            "apply_flag_settings" => self.apply_flag_settings(params).await,
            "set_permission_mode" => self.set_permission_mode(params),
            "set_session_thinking" => self.set_session_thinking(params).await,
            "output_styles_list" => self.output_styles_list(params).await,
            "set_output_style" => self.set_output_style(params).await,
            "reload_output_styles" => self.reload_output_styles(params).await,
            "commands_list" => self.commands_list(params).await,
            "reload_config" => self.reload_config().await,
            "reload_extensions" => self.reload_extensions(params),
            "prepare_weixin_login" => self.prepare_weixin_login().await,
            "dispatch_im_inbound_slash_command" => {
                Ok(self.dispatch_im_inbound_slash_command(params))
            }
            _ => return None,
        })
    }
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<JsValue> {
        self.notices.subscribe()
    }
    async fn config_changed(&mut self, paths: &[JsString]) {
        use super::changes::{ChangeClass, classify_config_changes};
        use crate::runtime::refresh::RefreshRouter;
        let classes = classify_config_changes(paths);
        if classes.is_empty() || classes.iter().all(|c| *c == ChangeClass::RestartRequired) {
            return;
        }
        let payload = crate::runtime::sdk_session::SdkConfigChangePayload {
            changed_paths: paths
                .iter()
                .map(|s| String::from_utf16_lossy(s.units()))
                .collect(),
            change_classes: classes.iter().map(|c| c.as_str().to_owned()).collect(),
        };
        for session in self.sdk.handlers().config_change_sessions() {
            if let Some(handler) = self.sdk.handlers().config_change_handler(&session) {
                let _ =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(&payload)));
            }
        }
        let payload = object([
            ("changedPaths", strings(paths.iter().cloned())),
            ("changeClasses", strings(classes.iter().map(|c| c.as_str()))),
        ]);
        if super::changes::is_run_policy_only_change(paths) {
            self.notify(payload);
            return;
        }
        let Ok(reload) = self.resolver.queue_reload() else {
            return;
        };
        self.output_styles.clear();
        let owner = self.session_owner.clone();
        let notices = self.notices.clone();
        let home = self.home.clone();
        let cwd = self.cwd.clone();
        let scheduler = self.memory_scheduler.clone();
        let store = self.store.clone();
        self.refresh_tasks.spawn(async move {
            let mut reload = reload;
            match reload.as_mut().await {
                Ok(()) => {
                    if let Some(owner) = owner { owner.mark_all_dirty("config_changed"); }
                    if let Some(scheduler) = scheduler {
                        let snapshot = store.snapshot();
                        let enabled = snapshot
                            .get("config")
                            .and_then(|c| c.get("memory"))
                            .and_then(|m| m.get("enabled"))
                            .and_then(JsValue::as_bool)
                            .unwrap_or(true);
                        let llm = crate::runtime::memory::edgeclaw::native_llm_from_snapshot(&snapshot);
                        let has_model = llm.is_some();
                        if let Some(llm) = llm {
                            scheduler.set_llm(llm);
                        }
                        scheduler.on_config_reload(|| (), None, enabled, has_model);
                    }
                    let notice = object([("type", "notification".into()), ("name", "config_changed".into()), ("payload", payload)]);
                    let _ = notices.send(notice.clone());
                    native::publish(&home,&cwd,notice);
                }
                Err(error) => eprintln!("[pilotdeck] Config runtime refresh rejected; keeping previous runtimes: {error}"),
            }
        });
    }
    fn notify(&self, payload: JsValue) {
        let notice = object([
            ("type", "notification".into()),
            ("name", "config_changed".into()),
            ("payload", payload),
        ]);
        let _ = self.notices.send(notice.clone());
        native::publish(&self.home, &self.cwd, notice);
    }
    async fn update_settings(&mut self, params: &JsValue) -> Result {
        if text_field(params, "source").as_deref() != Some("localSettings") {
            return Err(RouteError::new(
                "INVALID_SETTINGS_SOURCE",
                "Only localSettings is supported.",
            ));
        }
        if field(params, "settings").as_object().is_none() {
            return Err(RouteError::new(
                "INVALID_LOCAL_SETTINGS",
                "settings must be an object.",
            ));
        }
        if self.organization_policy.deny_local_settings_updates {
            return Err(RouteError::new(
                "GATEWAY_ORGANIZATION_SETTINGS_UPDATE_DENIED",
                "Gateway organization policy denies SDK localSettings updates.",
            ));
        }
        let (applied, cleared) = local_settings::update(self, field(params, "settings"))?;
        let mut changed = Vec::new();
        if !applied.is_empty() || !cleared.is_empty() {
            let before = self.store.snapshot();
            let after = self
                .store
                .reload("sdk-update-settings")
                .await
                .map_err(|e| RouteError::from(e.as_ref()))?;
            changed = super::changes::diff_config_snapshots(&before, &after);
            self.config_changed(&changed).await;
        }
        Ok(object([
            ("applied", strings(applied)),
            ("cleared", strings(cleared)),
            ("changedPaths", strings(changed)),
        ]))
    }
}

impl ConfigRoutes {
    pub(crate) async fn handle_webui(&mut self, method: &str, params: &JsValue) -> Option<Result> {
        Some(match method {
            "webui_config_get" => self.webui_config_get(),
            "webui_config_validate" => self.webui_config_validate(params),
            "webui_config_save" => self.webui_config_save(params).await,
            _ => return None,
        })
    }

    fn webui_path(&self) -> PathBuf {
        crate::metadata::resolve(&self.cwd, &super::global_config_path(&self.home, &self.env))
    }

    fn user_home(&self) -> PathBuf {
        self.env
            .get("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| self.home.parent().map(PathBuf::from))
            .unwrap_or_else(|| self.home.clone())
    }

    fn webui_record(&self) -> Result<JsValue> {
        let path = self.webui_path();
        let (exists, _, raw) = super::webui::read_file(&path)
            .map_err(|error| RouteError::new(error.code, error.message))?;
        let validation = self.webui_validation(&raw)?;
        let normalized = field(&validation, "config").clone();
        Ok(object([
            ("exists", JsValue::Bool(exists)),
            (
                "path",
                JsValue::String(path.to_string_lossy().into_owned().into()),
            ),
            ("config", super::snapshot::redact(&normalized)),
            (
                "validation",
                object([
                    ("valid", field(&validation, "valid").clone()),
                    ("errors", field(&validation, "errors").clone()),
                    ("warnings", field(&validation, "warnings").clone()),
                ]),
            ),
        ]))
    }

    fn webui_config_get(&self) -> Result {
        self.webui_record()
    }

    fn webui_config_validate(&self, params: &JsValue) -> Result {
        let config = field(params, "config");
        if config.as_object().is_none() {
            return Err(RouteError::new(
                "INVALID_WEBUI_CONFIG",
                "config must be an object",
            ));
        }
        self.webui_validation(config)
    }

    fn webui_validation(&self, config: &JsValue) -> Result {
        let normalized = super::webui::normalize(config, &self.home, &self.user_home());
        let mut errors = super::webui::validation_errors(&normalized);
        let mut warnings = Vec::new();
        let source = pilotdeck_protocol::stringify_json(&normalized)
            .map_err(|error| RouteError::new("INVALID_WEBUI_CONFIG", error.to_string()))?
            .ok_or_else(|| RouteError::new("INVALID_WEBUI_CONFIG", "config must be defined"))?;
        let diagnostics =
            match super::webui::validate_source(&source, &self.home, &self.cwd, &self.env) {
                Ok(diagnostics) => diagnostics,
                Err(error) if !error.diagnostics.is_empty() => error.diagnostics,
                Err(error) => {
                    errors.push(String::from_utf16_lossy(error.message.units()));
                    vec![]
                }
            };
        for diagnostic in diagnostics {
            let message = field(&diagnostic, "message")
                .as_string()
                .map(|message| String::from_utf16_lossy(message.units()))
                .unwrap_or_else(|| "Invalid configuration".into());
            if field(&diagnostic, "severity")
                .as_string()
                .is_some_and(|severity| severity.equals_str("warning"))
            {
                warnings.push(message);
            } else {
                errors.push(message);
            }
        }
        Ok(object([
            ("valid", JsValue::Bool(errors.is_empty())),
            ("errors", strings(errors)),
            ("warnings", strings(warnings)),
            ("config", super::snapshot::redact(&normalized)),
        ]))
    }

    async fn webui_config_save(&mut self, params: &JsValue) -> Result {
        let config = field(params, "config");
        if config.as_object().is_none() || field(config, "webui").as_object().is_none() {
            return Err(RouteError::new(
                "INVALID_WEBUI_CONFIG",
                "config.webui must be an object",
            ));
        }
        let normalized = super::webui::normalize(config, &self.home, &self.user_home());
        let errors = super::webui::validation_errors(&normalized);
        if !errors.is_empty() {
            return Err(RouteError::new(
                "WEBUI_CONFIG_VALIDATION_FAILED",
                errors.join("; "),
            ));
        }

        let path = self.webui_path();
        let (_, source, _) = super::webui::read_file(&path)
            .map_err(|error| RouteError::new(error.code, error.message))?;
        let source = if source.is_empty() { "{}\n" } else { &source };
        let mut candidate =
            local_settings::edit(source, &["webui".into()], field(&normalized, "webui"))?;
        if !field(config, "customEnv").is_undefined() {
            candidate = local_settings::edit(
                &candidate,
                &["customEnv".into()],
                field(config, "customEnv"),
            )?;
        }
        let before = self.store.snapshot();
        super::webui::commit_file(&path, &candidate, &self.home, &self.cwd, &self.env)
            .map_err(|error| RouteError::new(error.code, error.message))?;
        let after = self
            .store
            .reload("webui-save")
            .await
            .map_err(|error| RouteError::from(error.as_ref()))?;
        let changed = super::changes::diff_config_snapshots(&before, &after);
        self.config_changed(&changed).await;
        let mut record = self.webui_record()?;
        if let JsValue::Object(ref mut record) = record {
            record.insert("changedPaths".into(), strings(changed));
        }
        Ok(record)
    }
}

impl ConfigRoutes {
    fn resolve_settings(&self) -> JsValue {
        let snapshot = self.store.snapshot();
        object([
            ("schemaVersion", field(&snapshot, "schemaVersion").clone()),
            ("version", field(&snapshot, "version").clone()),
            ("loadedAt", field(&snapshot, "loadedAt").clone()),
            ("contentHash", field(&snapshot, "contentHash").clone()),
            (
                "config",
                super::snapshot::redact(field(&snapshot, "config")),
            ),
            ("sources", field(&snapshot, "sources").clone()),
            ("diagnostics", field(&snapshot, "diagnostics").clone()),
        ])
    }
}

impl ConfigRoutes {
    async fn apply_flag_settings(&mut self, params: &JsValue) -> Result {
        use crate::runtime::sdk_session::routes::{session_key, validate_flags};
        let key = session_key(params)?;
        let settings = field(params, "settings");
        if settings.as_object().is_none() {
            return Err(RouteError::new(
                "INVALID_FLAG_SETTINGS",
                "settings must be an object.",
            ));
        }
        self.check_busy(&key, "Cannot apply flag settings during an active turn.")?;
        validate_flags(settings)?;
        let result = self
            .sdk
            .apply_flag_settings(&key, settings)
            .map_err(|e| RouteError::new(e.code, e.message))?;
        if !result.applied.is_empty() || !result.cleared.is_empty() {
            self.close_session(&key).await?;
        }
        Ok(object([
            ("applied", strings(result.applied)),
            ("cleared", strings(result.cleared)),
        ]))
    }
}

impl ConfigRoutes {
    fn set_permission_mode(&mut self, params: &JsValue) -> Result {
        use crate::runtime::sdk_session::routes::{permission_mode, session_key};
        let mode = field(params, "mode");
        if !permission_mode(mode) {
            return Err(RouteError::new(
                "INVALID_PERMISSION_MODE",
                format!(
                    "Unsupported permission mode: {}",
                    crate::js_value::display(mode).unwrap_or_default()
                ),
            ));
        }
        let raw_key = field(params, "sessionKey")
            .as_string()
            .cloned()
            .unwrap_or_default();
        self.check_busy(
            &raw_key,
            "Cannot change permission mode during an active turn.",
        )?;
        self.permission_modes.lock().unwrap().set(
            &raw_key,
            super::permission::PermissionMode::new(
                mode.as_string().expect("validated mode").clone(),
            ),
        );
        let key = session_key(params)?;
        self.check_busy(&key, "Cannot change permission mode during an active turn.")?;
        self.sdk
            .apply_flag_settings(
                &key,
                &object([("permissions", object([("defaultMode", mode.clone())]))]),
            )
            .map_err(|e| RouteError::new(e.code, e.message))?;
        Ok(object([("applied", JsValue::Bool(true))]))
    }
}

impl ConfigRoutes {
    async fn set_session_thinking(&mut self, params: &JsValue) -> Result {
        use crate::runtime::sdk_session::routes::{session_key, validate_thinking};
        let key = session_key(params)?;
        self.check_busy(&key, "Cannot change thinking while a turn is active.")?;
        let thinking = field(params, "thinking");
        validate_thinking(thinking)?;
        self.sdk.set_session_thinking(&key, Some(thinking));
        self.close_session(&key).await?;
        Ok(object([("applied", JsValue::Bool(true))]))
    }
}

pub(super) fn timestamp() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default();
    pilotdeck_transport::system_clock::format_epoch_iso(ms)
}
impl ConfigRoutes {
    pub(super) fn sync_runtime_env_from_snapshot(&mut self, snapshot: &JsValue) {
        let user_home = super::webui::user_home_from_options(&self.home, &self.env);
        self.env = super::webui::merge_runtime_env(
            field(snapshot, "config"),
            &self.home,
            &user_home,
            &self.env,
        );
    }

    pub fn trigger_memory_config_reload(&self, snapshot: &JsValue) {
        if let Some(scheduler) = &self.memory_scheduler {
            let enabled = snapshot
                .get("config")
                .and_then(|c| c.get("memory"))
                .and_then(|m| m.get("enabled"))
                .and_then(JsValue::as_bool)
                .unwrap_or(true);
            let llm = crate::runtime::memory::edgeclaw::native_llm_from_snapshot(snapshot);
            let has_model = llm.is_some();
            if let Some(llm) = llm {
                scheduler.set_llm(llm);
            }
            scheduler.on_config_reload(|| (), None, enabled, has_model);
        }
    }

    async fn reload_config(&mut self) -> Result {
        let before = self.store.snapshot();
        let after = self
            .store
            .reload("rpc")
            .await
            .map_err(|e| RouteError::from(e.as_ref()))?;
        self.sync_runtime_env_from_snapshot(&after);
        let paths = super::changes::diff_config_snapshots(&before, &after);
        self.config_changed(&paths).await;
        self.trigger_memory_config_reload(&after);
        Ok(object([
            ("reloaded", JsValue::Bool(true)),
            ("changedPaths", strings(paths)),
        ]))
    }
}

impl ConfigRoutes {
    fn reload_extensions(&mut self, params: &JsValue) -> Result {
        use crate::runtime::refresh::RefreshRouter;
        let project = styles::project_key(params)?;
        let paths = match field(params, "changedPaths") {
            JsValue::Undefined | JsValue::Null => JsValue::Array(vec![]),
            JsValue::Array(_) => field(params, "changedPaths").clone(),
            _ => {
                return Err(RouteError::new(
                    "gateway_request_failed",
                    "changedPaths.join is not a function",
                ));
            }
        };
        self.resolver.invalidate(project.as_ref());
        if let Some(project) = project.as_ref().filter(|s| !s.is_empty()) {
            let root = PathBuf::from(String::from_utf16_lossy(project.units()));
            self.output_styles.remove(&root);
            if let Some(owner) = &self.session_owner {
                owner.mark_project_dirty(project, "extension_changed");
            }
        } else {
            self.output_styles.clear();
            if let Some(owner) = &self.session_owner {
                owner.mark_all_dirty("extension_changed");
            }
        }
        self.notify(object([
            ("changedPaths", paths.clone()),
            ("changeClasses", strings(["extension-changed"])),
        ]));
        if self
            .env
            .get("STAFFDECK_PROJECT_BINDING_ENABLED")
            .map(String::as_str)
            == Some("true")
        {
            use crate::runtime::sdk_session::SdkRuntimeResolver;
            if let Some(key) = project.as_ref().filter(|s| !s.is_empty()) {
                self.resolver
                    .resolve(Some(key))
                    .map_err(|e| RouteError::new(e.code, e.message))?;
            }
        }
        Ok(object([
            ("reloaded", JsValue::Bool(true)),
            ("changedPaths", paths),
        ]))
    }
}

/// The native channel host owns hot-start, persisted channel state and login I/O.
/// Absence is the baseline's supported `requested:false` branch.
pub trait WeixinLoginHost: Send + Sync {
    fn shutdown(&self) -> ChannelCall<'_, ()>;
    fn hot_start(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>>;
}
impl ConfigRoutes {
    async fn prepare_weixin_login(&self) -> Result {
        let requested_at = timestamp();
        let Some(host) = &self.weixin_login else {
            return Ok(object([
                ("requested", JsValue::Bool(false)),
                ("requestedAt", requested_at.as_str().into()),
                ("reason", "unsupported".into()),
            ]));
        };
        host.hot_start().await?;
        Ok(object([
            ("requested", JsValue::Bool(true)),
            ("requestedAt", requested_at.as_str().into()),
        ]))
    }
}

/// Trusted assembly input, never accepted from a WebSocket request or config file.
#[derive(Clone, Default)]
pub struct OrganizationPolicy {
    deny_local_settings_updates: bool,
    sdk: crate::runtime::sdk_session::OrganizationPolicy,
}
impl OrganizationPolicy {
    pub fn deny_local_settings_updates() -> Self {
        Self {
            deny_local_settings_updates: true,
            sdk: Default::default(),
        }
    }
}
pub struct ConfigHost {
    pub session_owner: crate::runtime::session::SessionOwner,
    pub permission_modes:
        std::sync::Arc<std::sync::Mutex<super::permission::GatewaySessionPermissionModeRegistry>>,
    pub interactions:
        Option<std::sync::Arc<crate::runtime::interaction::GatewayInteractionCoordinator>>,
    pub dialog_project_keys:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<JsString, JsString>>>,
    pub organization_policy: OrganizationPolicy,
    pub weixin_login: Option<std::sync::Arc<dyn WeixinLoginHost>>,
}
impl ConfigHost {
    pub fn new(session_owner: crate::runtime::session::SessionOwner) -> Self {
        Self {
            session_owner,
            permission_modes: Default::default(),
            interactions: None,
            dialog_project_keys: Default::default(),
            organization_policy: Default::default(),
            weixin_login: None,
        }
    }
}
impl ConfigRoutes {
    pub fn set_organization_policy(&mut self, policy: OrganizationPolicy) {
        self.sdk.set_organization_policy(Some(policy.sdk.clone()));
        self.organization_policy = policy;
    }
    pub fn bind_host(&mut self, host: ConfigHost) {
        self.host_bound = true;
        self.session_owner = Some(host.session_owner);
        self.permission_modes = host.permission_modes;
        self.interactions = host.interactions;
        self.dialog_project_keys = host.dialog_project_keys;
        self.set_organization_policy(host.organization_policy);
        self.weixin_login = host.weixin_login;
    }
}

impl ConfigRoutes {
    /// The host may drain pending refreshes at session admission/shutdown.
    pub async fn settle_runtime_refresh(&mut self) {
        while let Some(result) = self.refresh_tasks.join_next().await {
            if let Err(error) = result {
                eprintln!("[pilotdeck] Config refresh task failed: {error}");
            }
        }
    }

    fn stop_watching(&mut self) {
        self.watcher.take();
        if let Some(subscription) = self.watch_subscription.take() {
            subscription.unsubscribe();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::memory::WebMemoryScheduler;

    #[test]
    fn trigger_memory_config_reload_updates_scheduler_flags_from_snapshot() {
        let scratch = crate::scratch::Scratch::new("routes-memory-reload");
        let root = scratch.path().to_path_buf();
        std::fs::write(
            root.join("pilotdeck.yaml"),
            "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n",
        )
        .unwrap();
        let scheduler = WebMemoryScheduler::with_workspace_root(root);
        let mut routes = ConfigRoutes::new(
            scratch.path().to_path_buf(),
            scratch.path().to_path_buf(),
            Default::default(),
        )
        .unwrap();
        routes.memory_scheduler = Some(std::sync::Arc::new(scheduler.clone()));

        // Snapshot 1: memory disabled
        let snap_disabled = object([(
            "config",
            object([("memory", object([("enabled", JsValue::Bool(false))]))]),
        )]);
        routes.trigger_memory_config_reload(&snap_disabled);

        // Snapshot 2: memory enabled with no model
        let snap_enabled = object([(
            "config",
            object([("memory", object([("enabled", JsValue::Bool(true))]))]),
        )]);
        routes.trigger_memory_config_reload(&snap_enabled);
    }
}