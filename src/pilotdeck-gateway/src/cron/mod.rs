//! Local automation composition, independent of WebSocket connection lifetime.
pub mod activity;
pub mod always_on;
pub mod cli_ws_client;
pub mod client;
pub mod http;
pub mod native;
pub mod runtime;
pub mod schedule;
pub mod tasks;
pub mod tools;
use always_on::{AlwaysOnError, AlwaysOnService};
use native::{ProductionHost, SessionPolicies};
use runtime::{BackgroundService, CronRuntime, NativeAlwaysOnAdapter};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::broadcast;

pub const METHODS: &[&str] = &[
    "cron_create",
    "cron_list",
    "cron_update",
    "cron_delete",
    "cron_stop",
    "cron_run_now",
    "always_on_apply",
    "always_on_abort",
    "always_on_rerun_plan",
    "background_tasks",
    "background_task_stop",
];
pub struct Automation {
    pub cron: Arc<CronRuntime>,
    pub always_on: Arc<AlwaysOnService>,
    pub background: Arc<BackgroundService>,
    pub host: Arc<ProductionHost>,
    pub owner: crate::runtime::session::SessionOwner,
    pub sdk: Option<Arc<tokio::sync::Mutex<crate::runtime::sdk_session::SdkSessionRegistry>>>,
    pub interactions: Arc<crate::runtime::interaction::GatewayInteractionCoordinator>,
    pub async_hooks: crate::runtime::async_hooks::AsyncHookRegistry,
    configured: bool,
    pub activity: Arc<activity::Activity>,
}
impl Automation {
    pub async fn new(
        home: PathBuf,
        cwd: PathBuf,
        sidecar: PathBuf,
    ) -> Result<Arc<Self>, AlwaysOnError> {
        Self::new_with_user_dialogs(home, cwd, sidecar, Arc::default()).await
    }

    pub async fn new_with_user_dialogs(
        home: PathBuf,
        cwd: PathBuf,
        sidecar: PathBuf,
        user_dialogs: Arc<crate::user_dialog::GatewayUserDialogs>,
    ) -> Result<Arc<Self>, AlwaysOnError> {
        use crate::runtime::turn::wiring::{self, BinarySidecarSpawner, CompositionOptions};
        let env = std::env::vars().collect();
        let config = if crate::config::has_execution_config(&home, &cwd, &env)
            .map_err(|e| AlwaysOnError::new("config_read_failed", e.to_string()))?
        {
            let snapshot =
                crate::config::load_snapshot(&home, &cwd, &env, None, 1.0, "").map_err(|e| {
                    AlwaysOnError::new(
                        "config_invalid",
                        String::from_utf16_lossy(e.message.units()),
                    )
                })?;
            native::from_js(&snapshot)?
        } else {
            serde_json::json!({"config":{}})
        };
        // The startup directory is the host's trusted project, not an RPC-supplied path.
        let project_dir = home
            .join("projects")
            .join(crate::metadata::project_id(&cwd));
        std::fs::create_dir_all(&project_dir)
            .map_err(|e| AlwaysOnError::new("project_registration_failed", e.to_string()))?;
        let marker = project_dir.join(".cwd");
        match std::fs::read_to_string(&marker) {
            Ok(existing) if crate::metadata::resolve(&cwd, existing.trim()) != cwd => {
                return Err(AlwaysOnError::new(
                    "project_registration_failed",
                    "Project identity collision.",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::write(marker, cwd.to_string_lossy().as_bytes()).map_err(|e| {
                    AlwaysOnError::new("project_registration_failed", e.to_string())
                })?;
            }
            Err(e) => {
                return Err(AlwaysOnError::new(
                    "project_registration_failed",
                    e.to_string(),
                ));
            }
        }
        let policies = SessionPolicies::default();
        let activity = Arc::new(activity::Activity::default());
        let background = Arc::new(BackgroundService::new(cwd.clone())?);
        let cron_slot = Arc::new(std::sync::Mutex::new(std::sync::Weak::new()));
        let provider_home = home.clone();
        let provider_cwd = cwd.clone();
        let composition = wiring::compose_with_user_dialogs(
            CompositionOptions {
                pilot_home: home.clone(),
                cwd: cwd.to_string_lossy().as_ref().into(),
                spawner: Arc::new(BinarySidecarSpawner {
                    executable: sidecar,
                    cwd: cwd.clone(),
                }),
                providers: Box::new(move |owner| {
                    wiring::disk_providers(provider_home, provider_cwd, owner)
                }),
                policies: policies.clone(),
                background: background.clone(),
                activity: activity.clone(),
                cron: cron_slot.clone(),
                failure_guard_enabled: false,
                abort_turn_timeout_ms: 30_000,
                server_version: env!("CARGO_PKG_VERSION").into(),
            },
            user_dialogs,
        )
        .map_err(|e| AlwaysOnError::new("native_host_failed", e.to_string()))?;
        let host = Arc::new(ProductionHost {
            turn: composition.turn,
            policies,
            notifications: broadcast::channel(4096).0,
            background: background.clone(),
            activity: activity.clone(),
            weixin_delivery: std::sync::Mutex::new(None),
            webhook_delivery: std::sync::Mutex::new(None),
            telegram_delivery: std::sync::Mutex::new(None),
            signal_delivery: std::sync::Mutex::new(None),
            dingtalk_delivery: std::sync::Mutex::new(None),
            discord_delivery: std::sync::Mutex::new(None),
            feishu_delivery: std::sync::Mutex::new(None),
            email_delivery: std::sync::Mutex::new(None),
            wecom_callback_delivery: std::sync::Mutex::new(None),
            sms_delivery: std::sync::Mutex::new(None),
            whatsapp_delivery: std::sync::Mutex::new(None),
            home_assistant_delivery: std::sync::Mutex::new(None),
            matrix_delivery: std::sync::Mutex::new(None),
            qq_delivery: std::sync::Mutex::new(None),
            wecom_delivery: std::sync::Mutex::new(None),
            slack_delivery: std::sync::Mutex::new(None),
            mattermost_delivery: std::sync::Mutex::new(None),
            bluebubbles_delivery: std::sync::Mutex::new(None),
            api_server_delivery: std::sync::Mutex::new(None),
        });
        let configured = config["config"]["cron"].is_object();
        let cron_config = if configured {
            config["config"]["cron"].clone()
        } else {
            serde_json::json!({"enabled":false})
        };
        let task_config = tasks::CronConfig::from_value(&cron_config)
            .map_err(|e| AlwaysOnError::new(e.code, e.message))?;
        let tasks = tasks::CronTasks::new(home.clone(), task_config)
            .map_err(|e| AlwaysOnError::new(e.code, e.message))?;
        let cron = CronRuntime::new(tasks, host.clone());
        *cron_slot.lock().unwrap() = Arc::downgrade(&cron);
        let always_on = Arc::new(AlwaysOnService::new(
            home,
            config["config"]["alwaysOn"].clone(),
            Arc::new(NativeAlwaysOnAdapter(host.clone())),
        ));
        activity.bind(&always_on);
        let service = Arc::new(Self {
            cron,
            always_on,
            background,
            host,
            owner: composition.owner,
            sdk: composition.sdk,
            interactions: composition.interactions,
            async_hooks: composition.async_hooks,
            configured,
            activity,
        });
        service.cron.start().await?;
        if let Err(error) = service.always_on.start().await {
            let _ = service.stop().await;
            return Err(error);
        }
        Ok(service)
    }
    pub fn handles(method: &str) -> bool {
        METHODS.contains(&method)
    }
    pub(crate) fn has_cron_provider(&self) -> bool {
        self.configured
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.host.notifications.subscribe()
    }
    pub async fn handle(&self, method: &str, params: &Value) -> Result<Value, AlwaysOnError> {
        if method.starts_with("cron_") && !self.configured {
            return Err(AlwaysOnError::new(
                "CAPABILITY_UNAVAILABLE",
                "Cron runtime is not configured on this gateway.",
            ));
        }
        if let Some(call) = self.cron.tasks().handle(method, params, self.cron.as_ref()) {
            return call
                .await
                .map_err(|e| AlwaysOnError::new(e.code, e.message));
        }
        if BackgroundService::handles(method) {
            return self.background.handle(method, params).await;
        }
        self.always_on.handle(method, params).await
    }
    pub async fn stop(&self) -> Result<(), AlwaysOnError> {
        let result = self.cron.stop().await;
        self.always_on.stop().await;
        let native = self
            .host
            .turn
            .shutdown(std::time::Duration::from_secs(30))
            .await
            .map_err(|message| AlwaysOnError::new("gateway_request_failed", message));
        self.background.stop().await;
        result.and(native)
    }
    pub async fn response(&self, request: &pilotdeck_protocol::JsValue) -> Vec<u8> {
        let id = request
            .get("id")
            .cloned()
            .unwrap_or(pilotdeck_protocol::JsValue::Null);
        let method = request
            .get("method")
            .and_then(pilotdeck_protocol::JsValue::as_string)
            .and_then(|v| v.to_utf8().ok())
            .unwrap_or_default();
        let result = match request.get("params") {
            Some(p) => native::from_js(p),
            None => Ok(serde_json::json!({})),
        };
        let result = match result {
            Ok(params) => self.handle(&method, &params).await,
            Err(e) => Err(e),
        };
        let mut reply = pilotdeck_protocol::JsObject::new();
        reply.insert("type".into(), "response".into());
        reply.insert("id".into(), id);
        reply.insert(
            "ok".into(),
            pilotdeck_protocol::JsValue::Bool(result.is_ok()),
        );
        match result {
            Ok(value) => {
                reply.insert("result".into(), native::to_js(&value).expect("JSON value"));
            }
            Err(e) => {
                reply.insert(
                    "error".into(),
                    native::to_js(&e.value()).expect("JSON error"),
                );
            }
        }
        pilotdeck_protocol::stringify_json(&pilotdeck_protocol::JsValue::Object(reply))
            .expect("JSON response")
            .expect("response")
            .into_bytes()
    }
}
