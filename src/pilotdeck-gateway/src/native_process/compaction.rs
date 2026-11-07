//! The product's original /compact presentation delegates one private command
//! to its owning backend. Only Kernel admits the operation and commits history.
use crate::{
    metadata::object,
    runtime::{
        session::{SessionContext, SessionOwner},
        turn::{
            admission::{CompactionAbort, SubmitCompactionRouter},
            submit::{SubmitError, SubmitFuture},
        },
    },
};
use pilotdeck_protocol::{JsString, JsValue};

pub(super) struct NativeCompactionRouter {
    pub owner: SessionOwner,
    pub options: super::Options,
}
impl SubmitCompactionRouter for NativeCompactionRouter {
    fn configured(&self, session: &JsString) -> bool {
        if let Some(handle) = self.owner.current_handle(session) {
            return handle.backend_kind() == "kernel";
        }
        let mut path = super::assembly::transcript_path(&self.options, session).into_os_string();
        path.push(".backend.json");
        std::fs::read(std::path::PathBuf::from(path))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|owner| owner["backend"] == "kernel")
    }
    fn compact<'a>(
        &'a self,
        session: &'a JsString,
        turn: &'a JsString,
        abort: &'a CompactionAbort,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        Box::pin(async move {
            let failure = |message: String| SubmitError::new("manual_compaction_failed", message);
            let _reservation = self
                .owner
                .begin_maintenance(session)
                .map_err(|error| failure(error.message))?
                .ok_or_else(|| failure("Session has an active turn or queued work.".into()))?;
            let handle = self
                .owner
                .get_or_create(SessionContext {
                    session_key: session.clone(),
                    project_key: Some(self.options.project.to_string_lossy().into_owned().into()),
                    channel_key: "gateway".into(),
                    allowed_tools: None,
                    disallowed_tools: None,
                })
                .await
                .map_err(|error| failure(error.message))?;
            let _activity = handle
                .begin_maintenance()
                .map_err(|error| failure(error.message))?;
            if abort.reason().is_some() {
                return Ok(object([("type", "aborted".into())]));
            }
            let operation = handle.backend_request(
                "kernel_manual_compact_v1".into(),
                object([("turnId", JsValue::String(turn.clone()))]),
            );
            tokio::pin!(operation);
            let result = tokio::select! {
                biased;
                reason=abort.aborted()=>{
                    handle.abort(&reason).map_err(|error|failure(error.message))?;
                    // The cancel acknowledgement is not a terminal/effect result.
                    // Keep observing this original operation, including Unknown.
                    operation.await
                }
                result=&mut operation=>result,
            }
            .map_err(|error| {
                let code = error
                    .js_error()
                    .and_then(|error| error.code.as_deref())
                    .unwrap_or("manual_compaction_failed");
                SubmitError::new(code, error.message.clone())
            })?;
            result.ok_or_else(|| failure("Native compact backend is unavailable.".into()))
        })
    }
}
