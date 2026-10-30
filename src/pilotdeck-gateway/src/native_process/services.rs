use super::{Options, assembly::Recorders};
use crate::{
    config::Env,
    metadata::object,
    runtime::{
        project::{ProjectResources, ProjectRuntimeRegistry},
        turn::{
            submit::{SubmitError, SubmitFuture, SubmitSnapshots},
            submit_host::{SubmitAttachmentLease, SubmitCwdService, SubmitHostServices},
        },
    },
    storage::RunRef,
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;

pub struct Services {
    pub options: Options,
    pub project: Arc<ProjectRuntimeRegistry>,
    pub env: Env,
    pub recorders: Recorders,
    pub owner: crate::runtime::session::SessionOwner,
}
fn failure(message: impl Into<String>) -> SubmitError {
    SubmitError::new("gateway_submit_failed", message)
}
fn unavailable(message: &str) -> SubmitError {
    SubmitError::new("CAPABILITY_UNAVAILABLE", message)
}
struct Cwd(std::path::PathBuf);
impl SubmitCwdService for Cwd {
    fn set(&self, _: &JsString, cwd: &JsString) -> Result<(), SubmitError> {
        if cwd
            .to_utf8()
            .is_ok_and(|cwd| crate::metadata::resolve(&self.0, &cwd) == self.0)
        {
            Ok(())
        } else {
            Err(unavailable(
                "The isolated native process cannot change its configured project.",
            ))
        }
    }
}
impl SubmitHostServices for Services {
    fn workspace_id(&self, input: &JsValue) -> Result<Option<JsString>, SubmitError> {
        if let Some(project) = input.get("projectKey").filter(|v| !v.is_undefined()) {
            let project = project
                .as_string()
                .and_then(|s| s.to_utf8().ok())
                .ok_or_else(|| failure("projectKey must be a valid path string."))?;
            if crate::metadata::resolve(&self.options.project, &project) != self.options.project {
                return Err(unavailable(
                    "The isolated native process serves only its configured project.",
                ));
            }
        }
        // The composition does not expose the SDK/model setter yet. Refuse
        // these before session admission instead of reporting a false selection.
        for key in ["modelSelection", "modelOverride", "sdkConfig"] {
            if input.get(key).is_some_and(|v| !v.is_undefined()) {
                return Err(unavailable(
                    "Per-turn model/SDK overrides are not installed in this isolated process.",
                ));
            }
        }
        Ok(None)
    }
    fn snapshots(&self) -> Option<Arc<dyn SubmitSnapshots>> {
        None
    }
    fn snapshots_required(&self) -> bool {
        false
    }
    fn cwd(&self) -> Option<Arc<dyn SubmitCwdService>> {
        Some(Arc::new(Cwd(self.options.project.clone())))
    }
    fn refresh(&self) -> SubmitFuture<'_, Result<(), SubmitError>> {
        Box::pin(async {
            let current = crate::runtime::project::NativeConfigSource::new(
                self.options.project.clone(),
                self.env.clone(),
                Some(self.options.home.clone()),
                None,
            );
            use crate::runtime::project::ConfigSource;
            let fresh = current
                .load(&self.options.project)
                .map_err(|e| failure(e.to_string()))?;
            let lease = self
                .project
                .acquire(None)
                .map_err(|e| failure(e.to_string()))?;
            let staged = lease
                .runtime()
                .resources()
                .get::<ProjectResources>()
                .ok_or_else(|| failure("Native project resources are missing."))?
                .context()
                .snapshot();
            if fresh.get("config") != staged.get("config") {
                self.project
                    .reload()
                    .await
                    .map_err(|e| failure(e.to_string()))?;
                use crate::runtime::refresh::RefreshRouter;
                self.owner.mark_all_dirty("config_changed");
            }
            Ok(())
        })
    }
    fn run_policy(&self) -> Option<JsValue> {
        let lease = self.project.acquire(None).ok()?;
        lease
            .runtime()
            .resources()
            .get::<ProjectResources>()?
            .context()
            .snapshot()
            .get("config")?
            .get("runPolicy")
            .cloned()
    }
    fn resolve_attachments<'a>(
        &'a self,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<SubmitAttachmentLease, SubmitError>> {
        Box::pin(async {
            Err(unavailable(
                "Upload attachment composition is not installed in this isolated process.",
            ))
        })
    }
    fn prepare_attachment_turn<'a>(
        &'a self,
        message: &'a JsValue,
        attachments: Vec<JsValue>,
        _: Option<&JsString>,
        _: Option<&JsString>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        Box::pin(async move {
            super::assembly::validate_profile(&self.options, &self.env)
                .await
                .map_err(|error| unavailable(&error.to_string()))?;
            if !attachments.is_empty() {
                return Err(unavailable(
                    "Attachment composition is not installed in this isolated process.",
                ));
            }
            if !matches!(message, JsValue::String(_)) {
                return Err(failure("message must be a string."));
            }
            Ok(object([("type", "text".into()), ("text", message.clone())]))
        })
    }
    fn resolve_model_selection<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        Box::pin(async move {
            let metadata = crate::metadata::Metadata {
                home: self.options.home.clone(),
                cwd: self.options.project.clone(),
                files: crate::metadata::NativeFiles,
            };
            let params = object([
                (
                    "projectKey",
                    JsValue::String(self.options.project.to_string_lossy().into_owned().into()),
                ),
                (
                    "sessionKey",
                    input
                        .get("sessionKey")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
            ]);
            let state =
                crate::session_model::execute(&metadata, &params, &self.env).map_err(|e| {
                    SubmitError::new(&e.code, String::from_utf16_lossy(e.message.units()))
                })?;
            let saved = state.get("saved").cloned().unwrap_or(JsValue::Undefined);
            if saved.get("mode") == Some(&"model".into()) {
                Ok(object([("source", "session".into()), ("selection", saved)]))
            } else {
                Ok(object([(
                    "source",
                    state
                        .get("effective")
                        .and_then(|v| v.get("source"))
                        .cloned()
                        .unwrap_or("default".into()),
                )]))
            }
        })
    }
    fn record_failure<'a>(
        &'a self,
        run: &'a RunRef,
        status: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        let recorder = self
            .recorders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&run.session_key)
            .and_then(std::sync::Weak::upgrade);
        let run = run.clone();
        let status = status.clone();
        Box::pin(async move {
            let recorder =
                recorder.ok_or_else(|| failure("Native session recorder is unavailable."))?;
            tokio::task::spawn_blocking(move || {
                recorder.writer().record_agent_status_message(
                    &run.session_key,
                    &run.run_id,
                    &status,
                )
            })
            .await
            .map_err(|e| failure(e.to_string()))?
            .map_err(|e| failure(e.to_string()))
        })
    }
    fn after_turn(&self, _: &RunRef) -> Result<(), SubmitError> {
        self.recorders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, recorder| recorder.strong_count() > 0);
        Ok(())
    }
    fn now_ms(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
            * 1000.0
    }
    fn abort_turn_timeout_ms(&self) -> u64 {
        self.options.drain_budget.as_millis() as u64
    }
}
