//! Project-owned snapshot services and session-owned event recorders.
//! TS constructs these in createLocalGateway and AgentSessionRuntimeBundle,
//! respectively; the native composition exposes them through project resources.
//! All IO is synchronous: callers must use a blocking worker and retain their
//! RuntimeLease for the lifetime of a session. No background writes need draining.

use super::{FamilyResource, Resource, RuntimeError, RuntimeResult, StageContext, StageSink};
use crate::runtime::snapshot::{SnapshotGateway, SnapshotProjects};
use crate::storage::{
    ContentAddressedWorkspaceSnapshotRecorder, JsonlTranscriptStore, LedgerLock, StoreResult,
    TranscriptKey, TranscriptRead, TranscriptStore, WorkspaceSnapshotProvider,
    create_workspace_snapshot_provider,
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
    /// Compose the unregistered U3 snapshot gateway from this project's one
    /// provider. `cwd` is the process cwd `path.resolve`/`.cwd` markers use;
    /// callers pass it explicitly so tests and the host agree. A project with
    /// no snapshot config keeps the `CAPABILITY_UNAVAILABLE` branch. The
    /// gateway is not added to `LOCAL_METHODS`: default dispatch still forwards
    /// `snapshot_*` to the TS host until A8 owns the WS entry.
    pub fn snapshot_gateway(&self, cwd: impl Into<PathBuf>) -> SnapshotGateway<SnapshotProjects> {
        SnapshotGateway::new(
            self.snapshots
                .as_ref()
                .map(|snapshots| snapshots.provider.clone()),
            SnapshotProjects::new(self.pilot_home.clone(), cwd),
            self.project_root.to_string_lossy().into_owned(),
        )
    }

    /// Composition without the project staging sink, for factory adoption
    /// tests (#1019) that call `create_session` directly.
    #[cfg(test)]
    pub(crate) fn for_tests(project_root: PathBuf, pilot_home: PathBuf) -> Self {
        Self {
            project_root,
            pilot_home,
            snapshots: None,
        }
    }

    /// Restore a recorder before publishing it to the session. The host supplies
    /// the session storage's resolved transcript path; `None` deliberately selects
    /// memory-only recording (AgentSessionRuntimeBundle.ts:125-142). No home or
    /// project fallback is invented. A canonical-path advisory lock enforces one
    /// participating writer across generations, until all `recorder.writer()`
    /// clones drop. Reuse that writer for every producer and hold the session's
    /// RuntimeLease. TS writers, unlocked stores and hard-link aliases do not
    /// honor this fence (A6.2-TRANSCRIPT-LOCK-001); stop them before opening.
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
        let (store, locked, transcript_path): (
            Arc<dyn TranscriptStore>,
            Option<Arc<JsonlTranscriptStore>>,
            PathBuf,
        ) = match transcript_path {
            Some(path) => {
                // The store retains this spelling for diagnostics while the
                // key uses the canonical path protected by its writer lock.
                let (store, path) =
                    JsonlTranscriptStore::open_locked(&path).map_err(RecorderError::Store)?;
                let locked = Arc::new(store);
                (
                    locked.clone() as Arc<dyn TranscriptStore>,
                    Some(locked),
                    path,
                )
            }
            None => (Arc::new(MemoryEvents::default()), None, PathBuf::new()),
        };
        let key = TranscriptKey {
            project_root: self.project_root.clone(),
            pilot_home: self.pilot_home.clone(),
            session_id,
            transcript_path,
        };
        let restored = store.read(&key, None).map_err(RecorderError::Store)?;
        // A size-limited read is not an empty log. Refuse to create a writable
        // recorder with a reset chain (same guard as JsonlLedgerStore::open).
        restored.ensure_writable().map_err(RecorderError::Store)?;
        let recorder = Recorder::new(store.clone(), key.clone());
        recorder.restore(&restored.entries)?;
        Ok(SessionRecording {
            recorder,
            restored,
            store,
            locked,
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
        // The project holds no open files. Sessions retain their own writer
        // locks, including across project disposal; all IO is synchronous.
        Box::pin(async { Ok(()) })
    }
}

pub struct SessionRecording {
    pub recorder: Recorder,
    /// The exact restored entries and diagnostics, before any new append.
    pub restored: TranscriptRead,
    store: Arc<dyn TranscriptStore>,
    /// The same locked store allocation, kept concrete so its ledger fence can
    /// move out to the factory's writer lease (#1019).
    locked: Option<Arc<JsonlTranscriptStore>>,
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
    /// Transfer this recording to the session factory's writer lease (#1019):
    /// the store keeps serving reads and appends, while its ledger fence
    /// leaves the store so the factory can own and release it with the
    /// writer drain. A memory-only recording has no fence to transfer. The
    /// handoff also retires this recording's writer (draining any in-flight
    /// append first): every retained `Arc` clone shares the retirement, so a
    /// stale holder's append is a typed rejection instead of a second sequence
    /// cache forking the chain under the factory's replacement writer.
    pub(crate) fn into_storage(
        self,
    ) -> (Arc<dyn TranscriptStore>, TranscriptKey, Option<LedgerLock>) {
        let fence = self.locked.as_ref().and_then(|store| store.take_fence());
        if !crate::mutants::on("adopt-clone-not-retired") {
            self.recorder.writer().retire();
        }
        (self.store, self.key, fence)
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
