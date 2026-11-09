//! Automation adapter over the production serial submit host.
use super::always_on::{AlwaysOnError, HostFuture, RunTools};
use super::runtime::{BackgroundService, NativeHost, NativeTurnRequest};
use crate::runtime::turn::gateway::TurnGateway;
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct SessionPolicy {
    pub cwd: PathBuf,
    pub permission_mode: &'static str,
    pub can_prompt: bool,
    pub excluded_tools: Vec<&'static str>,
    pub permission_rules: Value,
    pub tools: Option<Arc<RunTools>>,
}
pub type SessionPolicies = Arc<Mutex<HashMap<JsString, SessionPolicy>>>;

pub struct ProductionHost {
    pub feishu_delivery: Mutex<Option<Arc<crate::config::routes::feishu::FeishuChannel>>>,
    pub email_delivery: Mutex<Option<Arc<crate::config::routes::email::EmailChannel>>>,
    pub wecom_callback_delivery:
        Mutex<Option<Arc<crate::config::routes::wecom_callback::WeComCallbackChannel>>>,
    pub discord_delivery: Mutex<Option<Arc<crate::config::routes::discord::DiscordChannel>>>,
    pub dingtalk_delivery: Mutex<Option<Arc<crate::config::routes::dingtalk::DingTalkChannel>>>,
    pub sms_delivery: Mutex<Option<Arc<crate::config::routes::sms::SmsChannel>>>,
    pub whatsapp_delivery: Mutex<Option<Arc<crate::config::routes::whatsapp::WhatsAppChannel>>>,
    pub turn: TurnGateway,
    pub policies: SessionPolicies,
    pub notifications: broadcast::Sender<Value>,
    pub background: Arc<BackgroundService>,
    pub activity: Arc<super::activity::Activity>,
    pub weixin_delivery: Mutex<Option<Arc<crate::config::routes::WeixinDelivery>>>,
    pub webhook_delivery: Mutex<Option<Arc<crate::config::routes::channel_http::WebhookChannel>>>,
    pub telegram_delivery: Mutex<Option<Arc<crate::config::routes::telegram::TelegramChannel>>>,
    pub signal_delivery: Mutex<Option<Arc<crate::config::routes::signal::SignalChannel>>>,
    pub home_assistant_delivery:
        Mutex<Option<Arc<crate::config::routes::home_assistant::HomeAssistantChannel>>>,
    pub matrix_delivery: Mutex<Option<Arc<crate::config::routes::matrix::MatrixChannel>>>,
    pub qq_delivery: Mutex<Option<Arc<crate::config::routes::qq::QQChannel>>>,
    pub wecom_delivery: Mutex<Option<Arc<crate::config::routes::wecom::WeComChannel>>>,
    pub slack_delivery: Mutex<Option<Arc<crate::config::routes::slack::SlackChannel>>>,
    pub mattermost_delivery: Mutex<Option<Arc<crate::config::routes::mattermost::MattermostChannel>>>,
    pub bluebubbles_delivery: Mutex<Option<Arc<crate::config::routes::bluebubbles::BlueBubblesChannel>>>,
    pub api_server_delivery:
        Mutex<Option<Arc<crate::config::routes::channel_http::ApiServerChannel>>>,
}
pub fn to_js(value: &Value) -> Result<JsValue, AlwaysOnError> {
    parse_json(&value.to_string())
        .map_err(|e| AlwaysOnError::new("serialization_failed", e.to_string()))
}
pub fn from_js(value: &JsValue) -> Result<Value, AlwaysOnError> {
    let text = stringify_json(value)
        .map_err(|e| AlwaysOnError::new("serialization_failed", e.to_string()))?
        .ok_or_else(|| AlwaysOnError::new("serialization_failed", "Undefined native value."))?;
    serde_json::from_str(&text)
        .map_err(|e| AlwaysOnError::new("serialization_failed", e.to_string()))
}

impl ProductionHost {
    async fn deliver_to_key(
        &self,
        key: &str,
        delivery: &Value,
    ) -> Result<bool, AlwaysOnError> {
        let key = crate::config::routes::im_cron::cron_protocol_channel_key(key);
        macro_rules! deliver {
            ($slot:ident) => {{
                let channel = {
                    let guard = self.$slot.lock().unwrap();
                    guard.clone()
                };
                if let Some(channel) = channel {
                    return match channel.deliver(delivery).await {
                        Ok(value) => Ok(value),
                        Err(error) => Err(AlwaysOnError::new(error.code, error.message)),
                    };
                }
            }};
        }
        match key {
            "feishu" => deliver!(feishu_delivery),
            "weixin" => {
                let channel = {
                    let guard = self.weixin_delivery.lock().unwrap();
                    guard.clone()
                };
                if let Some(channel) = channel {
                    return match channel.deliver(delivery).await {
                        Ok(value) => Ok(value),
                        Err(error) => Err(AlwaysOnError::new(error.code, error.message)),
                    };
                }
            }
            "telegram" => deliver!(telegram_delivery),
            "discord" => deliver!(discord_delivery),
            "matrix" => deliver!(matrix_delivery),
            "signal" => deliver!(signal_delivery),
            "whatsapp" => deliver!(whatsapp_delivery),
            "dingtalk" => deliver!(dingtalk_delivery),
            "wecom_callback" => deliver!(wecom_callback_delivery),
            "email" => deliver!(email_delivery),
            "sms" => deliver!(sms_delivery),
            "homeassistant" => deliver!(home_assistant_delivery),
            "webhook" => deliver!(webhook_delivery),
            _ => {}
        }
        Ok(false)
    }
}

impl NativeHost for ProductionHost {
    fn prepare_task<'a>(&'a self, task: &'a Value) -> HostFuture<'a, ()> {
        Box::pin(async move {
            let session = task["sessionKey"]
                .as_str()
                .ok_or_else(|| AlwaysOnError::new("invalid_session", "Task has no sessionKey."))?;
            let project = task["projectKey"]
                .as_str()
                .ok_or_else(|| AlwaysOnError::new("invalid_project", "Task has no projectKey."))?;
            self.policies.lock().unwrap().insert(session.into(), SessionPolicy {
                cwd: project.into(), permission_mode: "bypassPermissions", can_prompt: false,
                excluded_tools: crate::config::session_overrides::UNATTENDED_SESSION_EXCLUDED_TOOLS.to_vec(),
                permission_rules: json!({}), tools: None,
            });
            Ok(())
        })
    }
    fn release_task<'a>(&'a self, task: &'a Value) -> HostFuture<'a, ()> {
        Box::pin(async move {
            let session = task["sessionKey"]
                .as_str()
                .ok_or_else(|| AlwaysOnError::new("invalid_session", "Task has no sessionKey."))?;
            self.close(session).await
        })
    }
    fn turn(&self, request: NativeTurnRequest) -> HostFuture<'_, Vec<Value>> {
        Box::pin(async move {
            let session: JsString = request.session_key.as_str().into();
            self.policies.lock().unwrap().insert(
                session.clone(),
                SessionPolicy {
                    cwd: request.cwd,
                    permission_mode: request.permission_mode,
                    can_prompt: request.can_prompt,
                    excluded_tools: request.excluded_tools,
                    permission_rules: request.permission_rules,
                    tools: request.tools,
                },
            );
            let input = to_js(
                &json!({"sessionKey": request.session_key,"projectKey": request.project_key,
                "runId":request.run_id,"channelKey":request.channel_key,"message":request.message,
                "mode":request.permission_mode,"timeoutMs":request.timeout_ms}),
            )?;
            let callback = request.on_event;
            let events = self
                .turn
                .automation_turn(
                    input,
                    Arc::new(move |event| {
                        from_js(event)
                            .and_then(|event| callback(&event))
                            .map_err(|error| {
                                let mut e =
                                    crate::runtime::connection::RequestError::new(error.message);
                                e.code = error.code;
                                e
                            })
                    }),
                )
                .await
                .map_err(|e| {
                    AlwaysOnError::new(e.code, String::from_utf16_lossy(e.message.units()))
                })?;
            events.iter().map(from_js).collect()
        })
    }
    fn abort<'a>(&'a self, session: &'a str, run: &'a str, reason: &'a str) -> HostFuture<'a, ()> {
        Box::pin(async move {
            self.turn
                .automation_abort(session.into(), run.into(), reason.into())
                .await
                .map_err(|e| {
                    AlwaysOnError::new(e.code, String::from_utf16_lossy(e.message.units()))
                })
        })
    }
    fn close<'a>(&'a self, session: &'a str) -> HostFuture<'a, ()> {
        Box::pin(async move {
            self.turn
                .automation_close(session.into())
                .await
                .map_err(|e| {
                    AlwaysOnError::new(e.code, String::from_utf16_lossy(e.message.units()))
                })?;
            self.policies
                .lock()
                .unwrap()
                .remove(&JsString::from(session));
            self.activity.disconnect(&format!("native:{session}"));
            Ok(())
        })
    }
    fn on_turn_event(&self, session: &str, channel: &str, event: &Value) {
        let _ = self.notifications.send(json!({"name":"always-on:turn-event","payload":{"sessionKey":session,"channelKey":channel,"event":event}}));
    }
    fn on_worktree_created(&self, run: &str, cwd: &std::path::Path) {
        let _ = self.notifications.send(json!({
            "name":"worktree_created",
            "payload":{"runId":run,"cwd":cwd}
        }));
    }
    fn on_worktree_removed(&self, cwd: &std::path::Path) {
        let _ = self.notifications.send(json!({
            "name":"worktree_removed",
            "payload":{"cwd":cwd}
        }));
    }
    fn on_phase_event(&self, event: &Value) {
        let _ = self
            .notifications
            .send(json!({"name":"cron:phase-event","payload":event}));
    }
    fn deliver_result<'a>(&'a self, delivery: &'a Value) -> HostFuture<'a, ()> {
        Box::pin(async move {
            let _ = self
                .notifications
                .send(json!({"name":"cron:result","payload":delivery}));
            let Some(key) = crate::config::routes::im_cron::cron_delivery_lookup_key(delivery)
            else {
                eprintln!(
                    "[cron] result delivery was not handled task={} run={}",
                    delivery["taskId"].as_str().unwrap_or("?"),
                    delivery["runId"].as_str().unwrap_or("?"),
                );
                return Ok(());
            };
            match self.deliver_to_key(key, delivery).await {
                Ok(true) => Ok(()),
                Ok(false) => {
                    eprintln!(
                        "[cron] result delivery was not handled task={} run={}",
                        delivery["taskId"].as_str().unwrap_or("?"),
                        delivery["runId"].as_str().unwrap_or("?"),
                    );
                    Ok(())
                }
                Err(error) => Err(error),
            }
        })
    }
}

/// Turn-independent resources selected by the owning session factory.
pub struct NativeSessionServices {
    pub tools: Option<Arc<RunTools>>,
    pub background: crate::runtime::background::BackgroundResources,
    pub writer: Arc<crate::storage::TranscriptWriter>,
    pub goal: Arc<crate::runtime::tools::builtin::goal::DurableGoalService>,
    pub plan_todo: Arc<crate::runtime::tools::builtin::todo_write::DurableTodoService>,
    pub cron: std::sync::Weak<super::runtime::CronRuntime>,
}
impl NativeSessionServices {
    pub fn open(
        home: &std::path::Path,
        cwd: &std::path::Path,
        project: &str,
        session: &JsString,
        tools: Option<Arc<RunTools>>,
        background: crate::runtime::background::BackgroundResources,
    ) -> std::io::Result<Self> {
        let chat_dir = crate::session_chat_dir(home, cwd, project);
        let storage = crate::storage::AsyncTranscriptAdapter::new(
            Arc::new(crate::storage::JsonlTranscriptStore::new()),
            Arc::new({
                let cwd = cwd.to_path_buf();
                move |key| {
                    crate::session_chat_dir(
                        &key.pilot_home,
                        &cwd,
                        &key.project_root.to_string_lossy(),
                    )
                }
            }),
        )
        .create_session_storage(crate::storage::SessionStorageInput {
            pilot_home: home.to_path_buf(),
            project_root: project.into(),
            session_id: session.clone(),
            now: None,
        })
        .map_err(|error| std::io::Error::other(error.to_string()))?;
        storage
            .restore()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        // Goal and todo state are session-scoped, but both services may be
        // shared by nested turns. Keep their durable roots next to the
        // managed transcript directory derived from the explicit home/cwd.
        let tool_state = chat_dir.join("tools");
        let goal = Arc::new(
            crate::runtime::tools::builtin::goal::DurableGoalService::open(
                tool_state.join("goals"),
            )?,
        );
        let plan_todo = Arc::new(
            crate::runtime::tools::builtin::todo_write::DurableTodoService::open(
                tool_state.join("todos"),
            )?,
        );
        Ok(Self {
            tools,
            background,
            writer: Arc::new(storage.transcript),
            goal,
            plan_todo,
            cron: std::sync::Weak::new(),
        })
    }
}
