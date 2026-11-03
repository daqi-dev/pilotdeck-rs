//! S6: runtime tools and statistics for the native request router.
//! TS baseline: `InProcessGateway.ts:2392-2421`,
//! `ProjectRuntimeRegistry.ts:1293-1347,1707-1751`.
//! Methods: `usage_snapshot`, `model_usage_snapshot`, `mcp_server_status`,
//! `rewind_files`, `background_task_stop` (unix), `background_tasks` (unix).
use super::super::requests::Requests;
use super::string;
use crate::{
    runtime::{
        connection::{Call, RequestError},
        project::{ProjectResources, Resource},
        rewind::{self, RewindFilesInput},
    },
    storage::{AsyncTranscriptAdapter, JsonlTranscriptStore},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;

/// The full method list wired by this family (background entries are unix-only).
#[cfg(unix)]
pub(super) const METHODS: &[&str] = &[
    "usage_snapshot",
    "model_usage_snapshot",
    "mcp_server_status",
    "rewind_files",
    "background_task_stop",
    "background_tasks",
];

#[cfg(not(unix))]
pub(super) const METHODS: &[&str] = &[
    "usage_snapshot",
    "model_usage_snapshot",
    "mcp_server_status",
    "rewind_files",
];

pub(super) const HANDLER: super::Handler = super::Handler::AfterAdmission(Requests::tools_request);

fn router_error() -> RequestError {
    RequestError::new("Native project router resources are missing.")
}

fn session_error(code: &'static str, message: &'static str) -> RequestError {
    RequestError {
        code: code.into(),
        message: message.into(),
        details: None,
        validation: None,
    }
}

/// Trim-validated required string — aligns with TS `?.trim()` checks.
fn required_trimmed(
    params: &JsValue,
    field: &str,
    code: &'static str,
    message: &'static str,
) -> Result<JsString, RequestError> {
    let s = string(params, field)?;
    if s.to_utf8().map(|u| u.trim().is_empty()).unwrap_or(false) {
        return Err(session_error(code, message));
    }
    Ok(s)
}

#[cfg(unix)]
fn background_error(e: crate::runtime::background::BackgroundProjectionError) -> RequestError {
    // Propagate requires_host failures as CAPABILITY_UNAVAILABLE;
    // all other codes surface their natural error.
    let code = if e.requires_host {
        "CAPABILITY_UNAVAILABLE".into()
    } else {
        e.code.unwrap_or_else(|| "gateway_request_failed".into())
    };
    RequestError {
        code,
        message: e.message,
        details: None,
        validation: None,
    }
}

impl Requests {
    pub(in crate::native_process) fn tools_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            match method.as_str() {
                "usage_snapshot" => self.tools_usage_snapshot(&params),
                "model_usage_snapshot" => self.tools_model_usage_snapshot(&params),
                "mcp_server_status" => self.tools_mcp_server_status(),
                "rewind_files" => self.tools_rewind_files(&params).await,
                #[cfg(unix)]
                "background_task_stop" => self.tools_background_task_stop(&params).await,
                #[cfg(unix)]
                "background_tasks" => self.tools_background_tasks(&params),
                _ => super::metadata::unavailable(method),
            }
        })
    }

    /// `ProjectRuntimeRegistry.usageSnapshotForSdk` via this process's router stats.
    fn tools_usage_snapshot(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let session_key = optional_session_key(params);
        let lease = self
            .project
            .acquire(None)
            .map_err(|e| RequestError::new(e.to_string()))?;
        let router_arc = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .ok_or_else(router_error)?
            .context()
            .resource(Resource::Router)
            .ok_or_else(router_error)?;
        let router = router_arc
            .as_any()
            .downcast_ref::<crate::runtime::project::model::RouterResources>()
            .ok_or_else(router_error)?;
        Ok(router.router.usage_snapshot(session_key.as_ref()))
    }

    /// `ProjectRuntimeRegistry.modelUsageSnapshotForSdk` via this process's router stats.
    fn tools_model_usage_snapshot(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let session_key = optional_session_key(params);
        let lease = self
            .project
            .acquire(None)
            .map_err(|e| RequestError::new(e.to_string()))?;
        let router_arc = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .ok_or_else(router_error)?
            .context()
            .resource(Resource::Router)
            .ok_or_else(router_error)?;
        let router = router_arc
            .as_any()
            .downcast_ref::<crate::runtime::project::model::RouterResources>()
            .ok_or_else(router_error)?;
        Ok(router.router.model_usage_snapshot(session_key.as_ref()))
    }

    /// `mcpServerStatusForSdk`: native isolated mode enforces no configured MCP
    /// servers (validate_profile_inner), so the status is always an empty list.
    fn tools_mcp_server_status(&self) -> Result<JsValue, RequestError> {
        Ok(crate::metadata::object([(
            "servers",
            JsValue::Array(vec![]),
        )]))
    }

    /// `rewindFilesForSdk` — `InProcessGateway.ts:2402-2408` contract.
    async fn tools_rewind_files(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let requests = self.clone();
        let params = params.clone();
        // Synchronous disk IO must not block the serial host thread.
        // begin_maintenance is acquired before has_active_turn so that no new
        // turn can start between the check and the IO: begin_turn checks the
        // maintenance flag under the same admission lock.  A concurrent rewind
        // (maintenance already set for this session) or an active turn both
        // return SESSION_BUSY; the guard is dropped when the closure exits.
        tokio::task::spawn_blocking(move || {
            let session_key = required_trimmed(
                &params,
                "sessionKey",
                "INVALID_SESSION_KEY",
                "sessionKey is required.",
            )?;
            let user_message_id = required_trimmed(
                &params,
                "userMessageId",
                "INVALID_MESSAGE_ID",
                "userMessageId is required.",
            )?;
            let _rewind_guard = requests
                .owner
                .begin_maintenance(&session_key)
                .map_err(|e| RequestError::new(e.to_string()))?
                .ok_or_else(|| session_error("SESSION_BUSY", "A rewind is already in progress."))?;
            let dry_run = params
                .get("dryRun")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let user_message_id = user_message_id
                .to_utf8()
                .map_err(|_| RequestError::new("userMessageId must be valid UTF-8."))?;
            let adapter = AsyncTranscriptAdapter::new(
                Arc::new(JsonlTranscriptStore::new()),
                Arc::new(|key: &crate::storage::ProjectKey| {
                    crate::session_chat_dir(
                        &key.pilot_home,
                        &key.project_root,
                        &key.project_root.to_string_lossy(),
                    )
                }),
            );
            let home = requests.options.home.clone();
            let project = requests.options.project.clone();
            let result = rewind::rewind_files(RewindFilesInput {
                adapter: &adapter,
                project_root: &project,
                pilot_home: &home,
                session_key: &session_key,
                user_message_id: &user_message_id,
                dry_run,
                now: None,
            });
            Ok(result)
        })
        .await
        .map_err(|e| RequestError::new(e.to_string()))
        .and_then(|result| result)
    }

    /// `stopBackgroundTaskForSdk` via `stop_background_task_in_registry`.
    #[cfg(unix)]
    async fn tools_background_task_stop(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let session_key = required_trimmed(
            params,
            "sessionKey",
            "INVALID_SESSION_KEY",
            "sessionKey is required.",
        )?;
        let task_id = required_trimmed(params, "taskId", "INVALID_TASK_ID", "taskId is required.")?;
        crate::runtime::background::stop_background_task_in_registry(
            &self.project,
            None,
            &session_key,
            &task_id,
        )
        .await
        .map(JsValue::Object)
        .map_err(background_error)
    }

    /// `backgroundTasksForSdk` via `background_tasks_in_registry`.
    #[cfg(unix)]
    fn tools_background_tasks(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let session_key = required_trimmed(
            params,
            "sessionKey",
            "INVALID_SESSION_KEY",
            "sessionKey is required.",
        )?;
        let task_id = params.get("taskId").cloned().unwrap_or(JsValue::Undefined);
        crate::runtime::background::background_tasks_in_registry(
            &self.project,
            None,
            &session_key,
            &task_id,
        )
        .map(JsValue::Object)
        .map_err(background_error)
    }
}

/// `sessionKey` parameter: optional, null-tolerant, undeclared → None.
fn optional_session_key(params: &JsValue) -> Option<JsString> {
    params
        .get("sessionKey")
        .filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))
        .and_then(JsValue::as_string)
        .cloned()
}
