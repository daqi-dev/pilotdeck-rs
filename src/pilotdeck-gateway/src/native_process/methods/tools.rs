//! S6: runtime tools and statistics for the native request router.
//! TS baseline: `InProcessGateway.ts:2392-2421`,
//! `ProjectRuntimeRegistry.ts:1293-1347,1707-1751`.
//! Methods: `usage_snapshot`, `model_usage_snapshot`, `mcp_server_status`,
//! `rewind_files`, `background_task_stop` (unix), `background_tasks` (unix).
use super::super::requests::Requests;
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

/// Trim-validated required string for the `!input.<field>?.trim()` gates of
/// this family.
///
/// #1604: delegates to the family-shared guard. The old shape here —
/// `string()` followed by Rust `str::trim` — diverged from TS in two ways: a
/// non-string answered `"<field> must be a string."` instead of the optional
/// chain's `input.<field>?.trim is not a function` TypeError, and a
/// U+FEFF-only value counted as non-blank although JS trim strips it.
/// It stays a named one-line seam on purpose: the five call sites pass their
/// own domain codes, and the divergence note belongs where they read it.
fn required_trimmed(
    params: &JsValue,
    field: &str,
    code: &'static str,
    message: &'static str,
) -> Result<JsString, RequestError> {
    super::require_text(params, field, code, message)
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
                "mcp_server_status" => self.tools_mcp_server_status(&params).await,
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

    /// `mcpServerStatusForSdk` (`ProjectRuntimeRegistry.ts:1293-1307`) via the
    /// #1319 shared SDK MCP config: the same registry the four control
    /// methods write, so a set server reads back `configured` / `disabled`.
    /// #1432 replaces the #1311 isolated-mode constant empty list, which
    /// predated #1319's dynamic writes.
    async fn tools_mcp_server_status(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        self.mcp
            .config()
            .mcp_server_status(params)
            .await
            .map_err(super::mcp::mcp_error)
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
            // #1623 方案 B: rewind_files derives its transcript and file-history
            // paths from the key (`create_session_storage` →
            // `sanitize_session_id`), so a lone-surrogate key reads and
            // restores the `s\u{FFFD}` session's files through the alias
            // (fix-r2 probe B, `_tmp/20261111-issue-1623/fix-r2/`). TS
            // rewindFilesForSdk has no sessionKey gate at all
            // (ProjectRuntimeRegistry.ts:1125-1131, measured
            // `probe_ts_gates.log`); the rejection is the registered
            // A7-W5-SESSION-KEY-SURROGATE-REJECT-001 divergence — the shared
            // guard keeps the #1604 blank rejection and adds the surrogate one.
            let session_key = super::require_session_key(&params)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn session_params(value: JsValue) -> JsValue {
        crate::metadata::object([("sessionKey", value)])
    }

    fn message(error: &RequestError) -> String {
        error
            .message
            .to_utf8()
            .expect("#1604 the guard's message is UTF-8")
    }

    /// #1604: TS `!input.sessionKey?.trim()` on a number does not
    /// short-circuit at `?.`; `.trim` is not a function and the boundary
    /// serialises the TypeError. The old `string()` shape answered
    /// `sessionKey must be a string.` instead.
    #[test]
    fn a_non_string_session_key_throws_the_optional_chain_type_error() {
        let error = required_trimmed(
            &session_params(JsValue::Number(7.0)),
            "sessionKey",
            "INVALID_SESSION_KEY",
            "sessionKey is required.",
        )
        .expect_err("#1604 a number is not a trimmable string");
        assert_eq!("gateway_request_failed", error.code);
        assert_eq!("input.sessionKey?.trim is not a function", message(&error));
    }

    /// #1604: U+FEFF is WhiteSpace to JS trim but not to Rust `str::trim`,
    /// so a BOM-only key is blank to TS and must fail the domain gate.
    #[test]
    fn a_bom_only_session_key_is_blank() {
        let error = required_trimmed(
            &session_params(JsValue::from("\u{feff}")),
            "sessionKey",
            "INVALID_SESSION_KEY",
            "sessionKey is required.",
        )
        .expect_err("#1604 a BOM-only key is blank after JS trim");
        assert_eq!("INVALID_SESSION_KEY", error.code);
        assert_eq!("sessionKey is required.", message(&error));
    }

    /// Control: an ordinary key still passes and keeps its UTF-16 units.
    #[test]
    fn a_real_session_key_passes_through() {
        let key = required_trimmed(
            &session_params(JsValue::from("a8/session")),
            "sessionKey",
            "INVALID_SESSION_KEY",
            "sessionKey is required.",
        )
        .expect("#1604 a non-blank string passes the guard");
        assert_eq!("a8/session", key.to_utf8().expect("#1604 key is UTF-8"));
    }

    // #1623 方案 B: rewind_files derives its transcript and file-history
    // paths from the key (`create_session_storage` → `sanitize_session_id`),
    // so pre-fix a lone-surrogate key read and restored the `s\u{FFFD}`
    // session's files through the alias — including the worktree write of
    // `FileHistoryStore::rewind` (fix-r2 probe B,
    // `_tmp/20261111-issue-1623/fix-r2/probe_rust_prefix.log`). TS
    // rewindFilesForSdk has no sessionKey gate at all
    // (ProjectRuntimeRegistry.ts:1125-1131, measured `probe_ts_gates.log`);
    // the request gate is the registered A7-W5-SESSION-KEY-SURROGATE-REJECT-001
    // divergence. The guard keeps the #1604 blank rejection and adds the
    // surrogate one.
    #[tokio::test]
    async fn rewind_files_rejects_lone_surrogate_session_key() {
        use crate::negative_assert_eq;
        let (_s, r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1310 fixture");
        let surrogate = JsString::from_utf16(vec![0x73, 0xd800]);
        let params = crate::metadata::object([
            ("sessionKey", JsValue::String(surrogate)),
            ("userMessageId", "m1".into()),
        ]);
        let error = r
            .tools_request("rewind_files".to_string(), params)
            .await
            .expect_err("#1623 rewind_files rejects the lone-surrogate key");
        negative_assert_eq!(
            error.code.as_str(),
            "INVALID_SESSION_KEY",
            "#1623 rewind_files rejects the lone-surrogate session key"
        );
        negative_assert_eq!(
            message(&error),
            crate::native_process::SESSION_KEY_SURROGATE_MESSAGE,
            "#1623 the rewind rejection is the shared guard's domain error"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #1623 controls: only the unpaired shape is refused. An explicit U+FFFD
    // key and a surrogate pair (an astral character) pass the gate and reach
    // the transcript read, which answers the ordinary no-checkpoint failure —
    // the gate must not become a key filter TS never had.
    #[tokio::test]
    async fn rewind_files_keeps_fffd_and_paired_surrogate_keys() {
        use crate::negative_assert_eq;
        let (_s, r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1310 fixture");
        for (label, units) in [
            ("explicit-fffd", vec![0x73, 0xfffd]),
            ("paired-surrogate", vec![0x73, 0xd800, 0xdc00]),
        ] {
            let params = crate::metadata::object([
                ("sessionKey", JsValue::String(JsString::from_utf16(units))),
                ("userMessageId", "m1".into()),
            ]);
            let result = r
                .tools_request("rewind_files".to_string(), params)
                .await
                .unwrap_or_else(|error| panic!("#1623 the {label} key passes the gate: {error:?}"));
            negative_assert_eq!(
                result.get("canRewind").and_then(JsValue::as_bool),
                Some(false),
                "#1623 the {label} key reaches the transcript read, not the gate"
            );
            let error_text = result
                .get("error")
                .and_then(JsValue::as_string)
                .map(|text| String::from_utf16_lossy(text.units()));
            negative_assert_eq!(
                error_text,
                Some(rewind::NO_CHECKPOINT.to_string()),
                "#1623 the {label} key answers the ordinary no-checkpoint failure"
            );
        }
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }
}
