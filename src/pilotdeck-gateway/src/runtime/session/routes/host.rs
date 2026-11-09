//! Native transcript ports over the factory's actual sessions and shared disk adapter.
mod session;
pub use super::live::LiveBindings;
use super::transcript::{SeedReadTarget, TranscriptFuture, TranscriptHost};
use crate::{
    config::Error,
    metadata::{Metadata, NativeFiles, resolve_process_path},
    runtime::{
        sdk_session::SdkSessionRegistry,
        session::{SessionContext, SessionOwner},
        turn::submit_host::validate_sdk_session_config,
    },
    storage::{AsyncTranscriptAdapter, SessionStorage, SessionStorageInput, sanitize_session_id},
};
use pilotdeck_protocol::{JsString, JsValue};
pub(crate) use session::bind;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct NativeTranscriptHost {
    pub owner: SessionOwner,
    pub adapter: Arc<AsyncTranscriptAdapter>,
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub live: Arc<LiveBindings>,
    pub sdk: Option<Arc<tokio::sync::Mutex<SdkSessionRegistry>>>,
}

impl TranscriptHost for NativeTranscriptHost {
    fn resolve_project(&self, params: &JsValue) -> Result<PathBuf, Error> {
        let requested = match params.get("projectKey") {
            None | Some(JsValue::Undefined | JsValue::Null) => self.cwd.clone(),
            Some(JsValue::String(value)) => PathBuf::from(value.to_utf8().map_err(failed)?),
            _ => {
                return Err(error(
                    "gateway_request_failed",
                    "projectKey must be a string.",
                ));
            }
        };
        let project = resolve_process_path(&self.cwd, &requested).map_err(failed)?;
        let metadata = Metadata {
            home: self.home.clone(),
            cwd: self.cwd.clone(),
            files: NativeFiles,
        };
        if project != self.home && !metadata.has_registered_project(&project).map_err(failed)? {
            return Err(error(
                "PROJECT_NOT_FOUND",
                format!("Unknown projectKey: {}", requested.display()),
            ));
        }
        Ok(project)
    }

    fn storage(
        &self,
        project_root: &Path,
        session_key: &JsString,
    ) -> Result<SessionStorage, Error> {
        let id = sanitize_session_id(session_key);
        if id == "." || id == ".." || session_key.units().contains(&0) {
            return Err(error("INVALID_SESSION_KEY", "Invalid sessionKey."));
        }
        self.adapter
            .create_session_storage(SessionStorageInput {
                project_root: project_root.into(),
                pilot_home: self.home.clone(),
                session_id: session_key.clone(),
                now: None,
            })
            .map_err(failed)
    }

    fn prepare_seed<'a>(&'a self, params: &'a JsValue) -> TranscriptFuture<'a, SeedReadTarget> {
        Box::pin(async move {
            let session = params
                .get("sessionKey")
                .and_then(JsValue::as_string)
                .ok_or_else(|| error("INVALID_SESSION_KEY", "sessionKey is required."))?;
            let project = self.resolve_project(params)?;
            if let Some(cwd) = params
                .get("workspaceCwd")
                .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
            {
                let cwd = cwd.as_string().ok_or_else(|| {
                    error("gateway_request_failed", "workspaceCwd must be a string.")
                })?;
                if !cwd.is_empty() {
                    let path = PathBuf::from(cwd.to_utf8().map_err(failed)?);
                    let cwd = resolve_process_path(&self.cwd, &path).map_err(failed)?;
                    self.live
                        .set_cwd(session, cwd.to_string_lossy().as_ref().into());
                    // An idle session's cwd is immutable in the sidecar config; recreate it.
                    if self.owner.current_handle(session).is_some() {
                        self.owner.close(session).await.map_err(failed)?;
                    }
                }
            }
            if let Some(config) = params.get("sdkSessionConfig").filter(|value| {
                !matches!(
                    value,
                    JsValue::Undefined | JsValue::Null | JsValue::Bool(false)
                )
            }) {
                validate_sdk_session_config(config)
                    .map_err(|failure| error(&failure.code, failure.message))?;
                let sdk = self.sdk.as_ref().ok_or_else(|| {
                    error(
                        "gateway_request_failed",
                        "Session SDK state is not configured.",
                    )
                })?;
                let mut sdk = sdk.lock().await;
                let project_key = project.to_string_lossy().as_ref().into();
                let changed = sdk
                    .set_session_config(session, config, Some(&project_key), None)
                    .await
                    .map_err(|failure| error(failure.code(), failure.message()))?;
                drop(sdk);
                if changed {
                    self.owner.close(session).await.map_err(failed)?;
                }
            }
            let handle = self
                .owner
                .get_or_create(SessionContext {
                    session_key: session.clone(),
                    project_key: Some(project.to_string_lossy().as_ref().into()),
                    channel_key: params
                        .get("channelKey")
                        .and_then(JsValue::as_string)
                        .cloned()
                        .unwrap_or_else(|| "api_server".into()),
                    allowed_tools: None,
                    disallowed_tools: None,
                })
                .await
                .map_err(failed)?;
            if !self.owner.is_current(session, &handle) {
                return Err(error(
                    "SESSION_BUSY",
                    "Session changed while preparing file read state.",
                ));
            }
            let live = self.live.current(session).ok_or_else(|| {
                error(
                    "SESSION_BUSY",
                    "Session was closed while preparing file read state.",
                )
            })?;
            Ok(SeedReadTarget {
                cwd: live.cwd.clone(),
                additional_working_directories: vec![],
                bypass_permissions: false,
                checkpoint: live.checkpoint.clone(),
            })
        })
    }

    fn record_live_status<'a>(&'a self, params: &'a JsValue) -> TranscriptFuture<'a, Option<bool>> {
        Box::pin(async move {
            let Some(session) = params.get("sessionKey").and_then(JsValue::as_string) else {
                return Ok(None);
            };
            let turn = params
                .get("turnId")
                .and_then(JsValue::as_string)
                .ok_or_else(|| {
                    error(
                        "invalid_turn_id",
                        "Session event turnId must be a non-empty string.",
                    )
                })?;
            let undefined = JsValue::Undefined;
            let _publication = self.live.publication.lock().await;
            let status = params.get("status").unwrap_or(&undefined);
            let recorded = if let Some(state) = self.live.current(session) {
                let current = self
                    .owner
                    .current_handle(session)
                    .ok_or_else(|| error("SESSION_BUSY", "Session writer is draining."))?;
                if current
                    .snapshot()
                    .get("liveIdentity")
                    .and_then(JsValue::as_string)
                    != Some(&state.identity)
                {
                    return Err(error("SESSION_BUSY", "Session writer changed."));
                }
                state.record_status(turn, status).map_err(failed)?
            } else {
                if self.owner.current_handle(session).is_some() {
                    return Err(error("SESSION_BUSY", "Session writer is draining."));
                }
                let project = self.resolve_project(params)?;
                let storage = self.storage(&project, session)?;
                let read = storage.read_transcript(None).map_err(failed)?;
                if read.diagnostics.iter().any(|d| d.severity == "error") {
                    return Err(error(
                        "SESSION_TRANSCRIPT_INVALID",
                        "Cannot append a status to a transcript with invalid entries.",
                    ));
                }
                let recorder =
                    crate::storage::recorder::Recorder::from_writer(Arc::new(storage.transcript));
                recorder.restore(&read.entries).map_err(failed)?;
                recorder
                    .writer()
                    .record_agent_status_message(session, turn, status)
                    .map_err(failed)?;
                true
            };
            Ok(Some(recorded))
        })
    }
}

pub(crate) fn error(code: &str, message: impl Into<JsString>) -> Error {
    Error {
        code: code.into(),
        message: message.into(),
        diagnostics: vec![],
        name: "Error",
        requires_host: false,
    }
}
fn failed(failure: impl std::fmt::Display) -> Error {
    error("gateway_request_failed", failure.to_string())
}
