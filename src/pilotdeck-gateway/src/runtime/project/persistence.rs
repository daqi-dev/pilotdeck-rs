//! Project-owned snapshot services and session-owned event recorders.
//! TS constructs these in createLocalGateway and AgentSessionRuntimeBundle,
//! respectively; the native composition exposes them through project resources.
//! All IO is synchronous: callers must use a blocking worker and retain their
//! RuntimeLease for the lifetime of a session. No background writes need draining.

use super::{FamilyResource, Resource, RuntimeError, RuntimeResult, StageContext, StageSink};
use crate::storage::{
    ContentAddressedWorkspaceSnapshotRecorder, JsonlTranscriptStore, StoreResult, TranscriptKey,
    TranscriptRead, TranscriptStore, WorkspaceSnapshotProvider, create_workspace_snapshot_provider,
    recorder::{Recorder, RecorderError, RecorderResult},
    resolve_workspace_snapshot_config,
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    any::Any,
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub(super) fn stage(context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
    let env = context.env();
    let snapshots = resolve_workspace_snapshot_config(|key| env.get(key).cloned())
        .map_err(|error| RuntimeError::new(error.to_string()))?
        .map(|config| WorkspaceSnapshots {
            recorder: ContentAddressedWorkspaceSnapshotRecorder::new(config.clone()),
            provider: create_workspace_snapshot_provider(config),
        });
    sink.register_resource(
        Resource::Persistence,
        Arc::new(PersistenceResources {
            project_root: context.project_root(),
            pilot_home: context.pilot_home(),
            snapshots,
        }),
    );
    Ok(())
}

/// Actual content-addressed capture and read/restore services, sharing one root.
pub struct WorkspaceSnapshots {
    pub recorder: ContentAddressedWorkspaceSnapshotRecorder,
    pub provider: WorkspaceSnapshotProvider,
}

/// Immutable project composition; never shares a mutable event recorder across
/// sessions. An absent snapshot config is explicit, not a successful capture.
pub struct PersistenceResources {
    project_root: PathBuf,
    pilot_home: PathBuf,
    pub snapshots: Option<WorkspaceSnapshots>,
}
impl PersistenceResources {
    /// Restore a recorder before publishing it to the session. The host supplies
    /// the session storage's resolved transcript path; `None` deliberately selects
    /// memory-only recording (AgentSessionRuntimeBundle.ts:125-142). No home or
    /// project fallback is invented. One active writer per path is required,
    /// including across generations: hold the session's RuntimeLease and reuse
    /// `recorder.writer()` for every producer of that session's events.
    ///
    /// A new disk store per session rechecks crash debris on each reopen. Missing
    /// files and malformed lines retain the reader's diagnostics; invalid event
    /// chains fail restoration rather than silently starting at sequence zero.
    pub fn create_session(
        &self,
        session_id: JsString,
        transcript_path: Option<PathBuf>,
    ) -> RecorderResult<SessionRecording> {
        let persistent = transcript_path.is_some();
        let key = TranscriptKey {
            project_root: self.project_root.clone(),
            pilot_home: self.pilot_home.clone(),
            session_id,
            transcript_path: transcript_path.unwrap_or_default(),
        };
        let store: Arc<dyn TranscriptStore> = if persistent {
            Arc::new(JsonlTranscriptStore::new())
        } else {
            Arc::new(MemoryEvents::default())
        };
        let restored = store.read(&key, None).map_err(RecorderError::Store)?;
        // A size-limited read is not an empty log. Refuse to create a writable
        // recorder with a reset chain (same guard as JsonlLedgerStore::open).
        if let Some(diagnostic) = restored
            .diagnostics
            .iter()
            .find(|d| d.code == "transcript_too_large")
        {
            return Err(RecorderError::State(diagnostic.message.clone()));
        }
        let recorder = Recorder::new(store.clone(), key.clone());
        recorder.restore(&restored.entries)?;
        Ok(SessionRecording {
            recorder,
            restored,
            store,
            key,
            persistent,
        })
    }
}
impl FamilyResource for PersistenceResources {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn dispose(&self) -> super::DisposeFuture<'_> {
        // Constructors hold no open files, and every append/capture completes
        // before returning. Session lifetime belongs to the lease holder.
        Box::pin(async { Ok(()) })
    }
}

pub struct SessionRecording {
    pub recorder: Recorder,
    /// The exact restored entries and diagnostics, before any new append.
    pub restored: TranscriptRead,
    store: Arc<dyn TranscriptStore>,
    key: TranscriptKey,
    persistent: bool,
}
impl SessionRecording {
    pub fn is_persistent(&self) -> bool {
        self.persistent
    }
    /// Reads the actual backing store, not the recorder's sequence cache.
    pub fn read(&self) -> RecorderResult<TranscriptRead> {
        self.store
            .read(&self.key, None)
            .map_err(RecorderError::Store)
    }
}

#[derive(Default)]
struct MemoryEvents(Mutex<Vec<JsValue>>);
impl TranscriptStore for MemoryEvents {
    fn append(&self, _: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(entry.clone());
        Ok(())
    }
    fn read(&self, _: &TranscriptKey, _: Option<u64>) -> StoreResult<TranscriptRead> {
        Ok(TranscriptRead {
            entries: self.0.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            diagnostics: Vec::new(),
        })
    }
}
