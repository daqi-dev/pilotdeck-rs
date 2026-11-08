//! Read-only JSONL history access. Reuses the native writer's path identity.
use crate::{
    runtime::session::transcript_locks::session_transcript_path,
    storage::{TranscriptRead, read_transcript},
};
use pilotdeck_protocol::JsString;
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use tokio::sync::Semaphore;

static READ_SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();

#[derive(Clone)]
pub struct HistoryFiles {
    pub home: PathBuf,
    pub project: PathBuf,
}
impl HistoryFiles {
    /// Read exactly the same session transcript path used by the native recorder.
    /// The storage reader applies its existing 50 MiB bound and diagnostics.
    pub async fn read(&self, session: &JsString) -> Result<TranscriptRead, String> {
        let path = session_transcript_path(&self.home, &self.project, session);
        let permit = READ_SLOTS
            .get_or_init(|| Arc::new(Semaphore::new(4)))
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| e.to_string())?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            read_transcript(path).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
}
