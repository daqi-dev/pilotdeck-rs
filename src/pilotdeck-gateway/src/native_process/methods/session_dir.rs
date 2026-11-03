//! S1 session directory / lifecycle wiring.
//!
//! TS references:
//!  - `InProcessGateway.ts:2022-2078` (list/resume/new/close/closeProject/record)
//!  - `InProcessGateway.ts:2211-2239` (rename/tag/delete)
//!  - `createLocalGateway.ts:1116-1152` (rename/tag/delete implementations)
//!  - `SessionRouter.ts:368-420` (list, recordAgentStatusMessage)
//!  - `GatewaySessionCatalog.ts:20-35` (cursor protocol over SessionCatalogPort)
use super::super::requests::Requests;
use crate::{
    metadata::object,
    runtime::{
        connection::{Call, RequestError},
        manager::{
            GatewaySessionCatalogConsumer, SessionCatalogListInput, TranscriptSessionCatalog,
        },
    },
    storage::{AsyncTranscriptAdapter, JsonlTranscriptStore, ProjectKey, SessionStorageInput},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;

pub(super) const METHODS: &[&str] = &[
    "list_sessions",
    "resume_session",
    "new_session",
    "close_session",
    "delete_session",
    "rename_session",
    "tag_session",
    "close_project_sessions",
    "record_agent_status_message",
];
pub(super) const HANDLER: super::Handler =
    super::Handler::AfterAdmission(Requests::session_dir_request);

fn gateway_error(code: &'static str, message: impl Into<String>) -> RequestError {
    RequestError {
        code: code.into(),
        message: message.into().into(),
        details: None,
        validation: None,
    }
}

/// Resolves `projectKey` from params, falling back to the configured project.
/// Returns the resolved path string.
fn resolve_project(requests: &Requests, params: &JsValue) -> Result<String, RequestError> {
    let project = params
        .get("projectKey")
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_else(|| requests.options.project.to_string_lossy().into_owned());
    if !requests.options.serves_project(&project) {
        return Err(gateway_error(
            "CAPABILITY_UNAVAILABLE",
            "The isolated native process serves only its configured project.",
        ));
    }
    Ok(project)
}

/// Validates that `sessionKey` is present and non-blank (TS `?.trim()` check).
fn require_session_key(params: &JsValue) -> Result<JsString, RequestError> {
    let key = params
        .get("sessionKey")
        .and_then(JsValue::as_string)
        .filter(|s| {
            s.to_utf8()
                .is_ok_and(|u| !crate::metadata::js_trim(&u).is_empty())
        })
        .cloned()
        .ok_or_else(|| gateway_error("INVALID_SESSION_KEY", "sessionKey is required."))?;
    Ok(key)
}

/// Build a transcript adapter for the configured project.
fn _make_adapter(requests: &Requests) -> AsyncTranscriptAdapter {
    let home = requests.options.home.clone();
    AsyncTranscriptAdapter::new(
        Arc::new(JsonlTranscriptStore::new()),
        Arc::new(move |key: &ProjectKey| {
            crate::session_chat_dir(
                &home,
                &key.project_root,
                &key.project_root.to_string_lossy(),
            )
        }),
    )
}

impl Requests {
    pub(in crate::native_process) fn session_dir_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            match method.as_str() {
                "list_sessions" => self.session_list(&params).await,
                "resume_session" => self.session_resume(&params),
                "new_session" => self.session_new(&params),
                "close_session" => self.session_close(&params).await,
                "delete_session" => self.session_delete(&params).await,
                "rename_session" => self.session_rename_or_tag(&params, false).await,
                "tag_session" => self.session_rename_or_tag(&params, true).await,
                "close_project_sessions" => self.session_close_project(&params).await,
                "record_agent_status_message" => self.session_record_status(&params).await,
                _ => unreachable!("session_dir_request called for unknown method"),
            }
        })
    }

    /// `InProcessGateway.listSessions` → `SessionRouter.list` → catalog path.
    ///
    /// Cursor is a base-10 integer offset string (`GatewaySessionCatalog.ts:24-25`).
    async fn session_list(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let project_root = resolve_project(self, params)?;
        let limit = params
            .get("limit")
            .and_then(JsValue::as_number)
            .filter(|n| n.is_finite() && *n > 0.0);
        let offset: f64 = params
            .get("cursor")
            .and_then(JsValue::as_string)
            .and_then(|s| s.to_utf8().ok())
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|n| n.is_finite())
            .unwrap_or(0.0);
        let home = self.options.home.clone();
        let input = SessionCatalogListInput {
            project_root: project_root.clone().into(),
            pilot_home: home.clone(),
            limit,
            offset: Some(offset),
            include_internal: false,
        };
        let sessions = tokio::task::spawn_blocking(move || {
            let adapter = AsyncTranscriptAdapter::new(
                Arc::new(JsonlTranscriptStore::new()),
                Arc::new(move |key: &ProjectKey| {
                    crate::session_chat_dir(
                        &home,
                        &key.project_root,
                        &key.project_root.to_string_lossy(),
                    )
                }),
            );
            let catalog = Arc::new(TranscriptSessionCatalog::new(Arc::new(adapter)));
            GatewaySessionCatalogConsumer::new(catalog).list(&input)
        })
        .await
        .map_err(|e| RequestError::new(e.to_string()))?
        .map_err(|e| RequestError {
            code: e.code().unwrap_or("gateway_request_failed").into(),
            message: e.message().clone(),
            details: None,
            validation: None,
        })?;
        let next_cursor = limit
            .filter(|lim| sessions.len() as f64 >= *lim)
            .map(|lim| (offset + lim).to_string());
        let mut result = pilotdeck_protocol::JsObject::new();
        result.insert("sessions".into(), JsValue::Array(sessions));
        if let Some(cursor) = next_cursor {
            result.insert("nextCursor".into(), JsValue::String(cursor.into()));
        }
        Ok(JsValue::Object(result))
    }

    /// `InProcessGateway.resumeSession` → identity (returns the key unchanged).
    fn session_resume(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let key = require_session_key(params)?;
        Ok(object([("sessionKey", JsValue::String(key))]))
    }

    /// `InProcessGateway.newSession` → generate key from channelKey + optional projectKey.
    fn session_new(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let channel_key = params
            .get("channelKey")
            .and_then(JsValue::as_string)
            .cloned()
            .ok_or_else(|| RequestError::new("channelKey must be a string."))?;
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let project_prefix = params
            .get("projectKey")
            .and_then(JsValue::as_string)
            .and_then(|s| s.to_utf8().ok())
            .filter(|s| !s.is_empty())
            .map(|p| format!("project={}:", p))
            .unwrap_or_default();
        let session_key = format!(
            "{}:{}s_{}",
            channel_key
                .to_utf8()
                .map_err(|_| RequestError::new("channelKey must be a valid UTF-8 string."))?,
            project_prefix,
            suffix,
        );
        Ok(object([(
            "sessionKey",
            JsValue::String(session_key.into()),
        )]))
    }

    /// `InProcessGateway.closeSession` → owner.close + interaction coordinator cleanup.
    async fn session_close(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let key = require_session_key(params)?;
        let reason: JsString = params
            .get("reason")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| "session_closed".into());
        self.owner
            .close(&key)
            .await
            .map_err(|e| RequestError::new(e.to_string()))?;
        self.interactions.close_session(&key, &reason);
        Ok(JsValue::Undefined)
    }

    /// `createLocalGateway.deleteSession` → close live owner, delete transcript +
    /// file history + tool results + subagents directories.
    async fn session_delete(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let key = require_session_key(params)?;
        let project_root: std::path::PathBuf = resolve_project(self, params)?.into();
        // Close the live session first so the writer drains before we delete.
        self.owner
            .close(&key)
            .await
            .map_err(|e| RequestError::new(e.to_string()))?;
        let home = self.options.home.clone();
        let key_clone = key.clone();
        let project_clone = project_root.clone();
        tokio::task::spawn_blocking(move || {
            let home2 = home.clone();
            let adapter = AsyncTranscriptAdapter::new(
                Arc::new(JsonlTranscriptStore::new()),
                Arc::new(move |k: &ProjectKey| {
                    crate::session_chat_dir(
                        &home2,
                        &k.project_root,
                        &k.project_root.to_string_lossy(),
                    )
                }),
            );
            let storage = adapter
                .create_session_storage(SessionStorageInput {
                    project_root: project_clone.clone(),
                    pilot_home: home.clone(),
                    session_id: key_clone.clone(),
                    now: None,
                })
                .map_err(|e| RequestError::new(e.to_string()))?;
            // Verify the transcript exists before deleting.
            let exists = storage
                .transcript_exists()
                .transpose()
                .map_err(|e| RequestError::new(e.to_string()))?
                .unwrap_or(false);
            if !exists {
                let msg = format!(
                    "Session not found: {}",
                    String::from_utf16_lossy(key_clone.units())
                );
                return Err(gateway_error("SESSION_NOT_FOUND", msg));
            }
            // Delete transcript files.
            if let Some(result) = storage.delete_session_transcripts() {
                result.map_err(|e| RequestError::new(e.to_string()))?;
            }
            // Remove file history and tool results directories.
            for dir in [
                &storage.file_history_dir,
                &storage.tool_results_dir,
                &storage.subagents_dir,
            ] {
                let _ = std::fs::remove_dir_all(dir);
            }
            Ok(())
        })
        .await
        .map_err(|e| RequestError::new(e.to_string()))??;
        Ok(JsValue::Undefined)
    }

    /// `createLocalGateway.renameSession` / `tagSession` → busy check + metadata record.
    /// `is_tag` selects between title (rename) and tag metadata fields.
    async fn session_rename_or_tag(
        &self,
        params: &JsValue,
        is_tag: bool,
    ) -> Result<JsValue, RequestError> {
        let key = require_session_key(params)?;
        if self.owner.has_active_turn(&key) {
            let verb = if is_tag { "tag" } else { "rename" };
            return Err(gateway_error(
                "SESSION_BUSY",
                format!("Cannot {verb} an active session."),
            ));
        }
        let project_root: std::path::PathBuf = resolve_project(self, params)?.into();
        let home = self.options.home.clone();
        let value_field: JsValue = params.get("value").cloned().unwrap_or(JsValue::Undefined);
        let value_str: Option<String> = match &value_field {
            JsValue::Null | JsValue::Undefined => None,
            other => other
                .as_string()
                .and_then(|s| s.to_utf8().ok())
                .or_else(|| {
                    // TS: `String(input.value)` — convert number/bool to string
                    match other {
                        JsValue::Number(n) => Some(n.to_string()),
                        JsValue::Bool(b) => Some(b.to_string()),
                        _ => None,
                    }
                }),
        };
        let key_clone = key.clone();
        tokio::task::spawn_blocking(move || {
            let home2 = home.clone();
            let adapter = AsyncTranscriptAdapter::new(
                Arc::new(JsonlTranscriptStore::new()),
                Arc::new(move |k: &ProjectKey| {
                    crate::session_chat_dir(
                        &home2,
                        &k.project_root,
                        &k.project_root.to_string_lossy(),
                    )
                }),
            );
            let storage = adapter
                .create_session_storage(SessionStorageInput {
                    project_root: project_root.clone(),
                    pilot_home: home.clone(),
                    session_id: key_clone.clone(),
                    now: None,
                })
                .map_err(|e| RequestError::new(e.to_string()))?;
            // Seed the writer's sequence from the existing transcript so the
            // metadata entry continues the chain rather than restarting at 1.
            if matches!(storage.transcript_exists(), Some(Ok(true))) {
                storage
                    .restore()
                    .map_err(|e| RequestError::new(e.to_string()))?;
            }
            let updated_at = pilotdeck_transport::system_clock::format_epoch_iso(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64,
            );
            let mut meta = pilotdeck_protocol::JsObject::new();
            let field_name = if is_tag { "tag" } else { "title" };
            meta.insert(
                field_name.into(),
                value_str
                    .map(|s| JsValue::String(s.into()))
                    .unwrap_or(JsValue::Undefined),
            );
            meta.insert("updatedAt".into(), JsValue::String(updated_at.into()));
            let turn_id = if is_tag { "sdk-tag" } else { "sdk-rename" };
            storage
                .transcript
                .record_session_metadata(
                    &key_clone,
                    &JsString::from(turn_id),
                    JsValue::Object(meta),
                )
                .map_err(|e| RequestError::new(e.to_string()))
        })
        .await
        .map_err(|e| RequestError::new(e.to_string()))??;
        Ok(object([("updated", JsValue::Bool(true))]))
    }

    /// `InProcessGateway.closeProjectSessions` → resume or close all project sessions.
    async fn session_close_project(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let project_key_str = params
            .get("projectKey")
            .and_then(JsValue::as_string)
            .and_then(|s| s.to_utf8().ok())
            .filter(|s| !crate::metadata::js_trim(s).is_empty())
            .ok_or_else(|| RequestError::new("projectKey is required."))?;
        if !self.options.serves_project(&project_key_str) {
            return Err(gateway_error(
                "CAPABILITY_UNAVAILABLE",
                "The isolated native process serves only its configured project.",
            ));
        }
        let project_key: JsString = project_key_str.into();
        if params.get("resume") == Some(&JsValue::Bool(true)) {
            self.owner.resume_project(&project_key);
            return Ok(object([("sessionKeys", JsValue::Array(vec![]))]));
        }
        let keys = self
            .owner
            .close_project(&project_key)
            .await
            .map_err(|e| RequestError::new(e.to_string()))?;
        for key in &keys {
            self.interactions
                .close_session(key, &"project_closed".into());
        }
        let session_keys: Vec<JsValue> = keys.into_iter().map(JsValue::String).collect();
        Ok(object([("sessionKeys", JsValue::Array(session_keys))]))
    }

    /// `InProcessGateway.recordAgentStatusMessage` → live owner first, cold fallback.
    ///
    /// Live path: look up the session's recorder in the shared recorders map.
    /// Cold path: append via a fresh transcript writer (like `options.recordAgentStatusMessage`).
    async fn session_record_status(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let session_key = params
            .get("sessionKey")
            .and_then(JsValue::as_string)
            .cloned()
            .ok_or_else(|| RequestError::new("sessionKey must be a string."))?;
        let turn_id = params
            .get("turnId")
            .and_then(JsValue::as_string)
            .cloned()
            .ok_or_else(|| RequestError::new("turnId must be a string."))?;
        let status = params.get("status").cloned().unwrap_or(JsValue::Undefined);

        // Live path: recorder registered by the active session writer.
        let live_recorder = self
            .recorders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&session_key)
            .and_then(std::sync::Weak::upgrade);
        if let Some(recorder) = live_recorder {
            let session_key2 = session_key.clone();
            let turn_id2 = turn_id.clone();
            let status2 = status.clone();
            let recorded = tokio::task::spawn_blocking(move || {
                recorder
                    .writer()
                    .record_agent_status_message(&session_key2, &turn_id2, &status2)
                    .map(|()| true)
            })
            .await
            .map_err(|e| RequestError::new(e.to_string()))?
            .map_err(|e| RequestError::new(e.to_string()))?;
            return Ok(object([("recorded", JsValue::Bool(recorded))]));
        }

        // Cold path: write to the durable transcript directly.
        let home = self.options.home.clone();
        let project_root = self.options.project.clone();
        let recorded = tokio::task::spawn_blocking(move || {
            let home2 = home.clone();
            let adapter = AsyncTranscriptAdapter::new(
                Arc::new(JsonlTranscriptStore::new()),
                Arc::new(move |k: &ProjectKey| {
                    crate::session_chat_dir(
                        &home2,
                        &k.project_root,
                        &k.project_root.to_string_lossy(),
                    )
                }),
            );
            match adapter.create_session_storage(SessionStorageInput {
                project_root: project_root.clone(),
                pilot_home: home.clone(),
                session_id: session_key.clone(),
                now: None,
            }) {
                Ok(storage) => {
                    // Seed the writer's sequence from the existing transcript so
                    // the status entry continues the chain rather than restarting
                    // at 1.
                    if matches!(storage.transcript_exists(), Some(Ok(true))) {
                        if storage.restore().is_err() {
                            return false;
                        }
                    }
                    storage
                        .transcript
                        .record_agent_status_message(&session_key, &turn_id, &status)
                        .map(|()| true)
                        .unwrap_or(false)
                }
                Err(_) => false,
            }
        })
        .await
        .unwrap_or(false);
        Ok(object([("recorded", JsValue::Bool(recorded))]))
    }
}
