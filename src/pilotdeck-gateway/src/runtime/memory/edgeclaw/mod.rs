//! EdgeClaw SQLite and file memory storage.
//!
//! Ports the storage subset of the fixed TS baseline
//! `src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts`
//! (`MemoryRepository`) that A6.6-pr10a owns:
//!
//! * open + `PRAGMA journal_mode = WAL` + schema and
//!   `pipeline_state` → `pipeline_state_v2` migration (:520-590)
//! * `pipeline_state` CRUD (:916-928)
//! * `l0_sessions` insert and pending-session queries (:930-1020)
//! * case/index/dream trace save/list/get (:1105-1166)
//! * indexing settings (:1168-1176)
//!
//! [`file_memory`] adds A6.6-pr10b FileMemoryStore, manifest, profile, metadata
//! and file export APIs, plus workspace/global file-store wiring. Repository
//! snapshot capture, dream rollback and root swaps live in [`storage::snapshots`].
//! Retrieval, extraction and heartbeat belong to the remaining work packages.
pub mod file_memory;
pub mod model;
mod node_fs;
pub mod provider;
pub mod storage;

pub use file_memory::{
    EdgeClawFileStores, FileMemoryError, FileMemoryStore, FileMemoryStoreOptions,
};

pub use model::native_llm_from_snapshot;
pub use provider::{
    NativeEdgeClawClock, NativeEdgeClawLlm, NativeEdgeClawLlmFuture, NativeEdgeClawOptions,
    NativeEdgeClawRuntime, NativeProjectMemoryOwner, create_project_provider,
    native_provider_factory, stage_native_from_snapshot,
};
pub use storage::{EdgeClawStorage, IndexingSettings, L0SessionRecord, StorageError};

/// The management surface uses the same native repository as extraction/storage.
pub struct DiskMemoryManagement(EdgeClawFileStores);
impl DiskMemoryManagement {
    pub fn open(
        project: &std::path::Path,
        root: &std::path::Path,
        general: &std::path::Path,
    ) -> Result<Self, super::MemoryProviderError> {
        let data = root
            .join("workspaces")
            .join(file_memory::helpers::hash(&project.to_string_lossy()));
        let store = EdgeClawFileStores::with_workspace_clock(
            data.join("control.sqlite"),
            data.join("memory"),
            root.join("global"),
            project,
            general,
            std::sync::Arc::new(|| {
                pilotdeck_transport::system_clock::format_epoch_iso(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis()
                        .min(i64::MAX as u128) as i64,
                )
            }),
        )
        .map_err(management_error)?;
        store
            .storage
            .set_pipeline_state("workspaceDir", &project.to_string_lossy().as_ref().into())
            .map_err(|e| super::MemoryProviderError::new(e.to_string()))?;
        Ok(Self(store))
    }
}
fn management_error(error: FileMemoryError) -> super::MemoryProviderError {
    super::MemoryProviderError::new(error.to_string())
}
impl super::MemoryManagementService for DiskMemoryManagement {
    fn list(
        &self,
        limit: usize,
    ) -> Result<Vec<pilotdeck_protocol::JsValue>, super::MemoryProviderError> {
        self.0.list_memory_entries(limit).map_err(management_error)
    }
    fn clear(&self, scope: &str) -> Result<(), super::MemoryProviderError> {
        if scope == "all_memory" {
            self.0.clear_all_memory_data()
        } else {
            self.0.clear_current_workspace_memory_data()
        }
        .map(|_| ())
        .map_err(management_error)
    }
    fn clear_session(
        &self,
        session: &pilotdeck_protocol::JsString,
    ) -> Result<(), super::MemoryProviderError> {
        let session = session
            .to_utf8()
            .map_err(|_| super::MemoryProviderError::host_required())?;
        self.0
            .clear_session_memory_data(&session)
            .map(|_| ())
            .map_err(management_error)
    }
}

/// Reads `workspaceDir` from `data_dir/control.sqlite` if present.
/// Aligns with `ui/server/services/memoryService.js:61-82`.
pub fn read_workspace_dir_from_data_dir(data_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let db_path = data_dir.join("control.sqlite");
    if !db_path.exists() {
        return None;
    }
    let storage = storage::EdgeClawStorage::open(&db_path).ok()?;
    let value = storage.get_pipeline_state("workspaceDir").ok()??;
    let s = match &value {
        pilotdeck_protocol::JsValue::String(s) => s.to_utf8().ok()?,
        _ => return None,
    };
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(trimmed))
    }
}

#[cfg(test)]
mod management_tests {
    use super::*;
    use crate::runtime::memory::MemoryManagementService;
    use pilotdeck_protocol::{JsValue, parse_json};

    #[test]
    fn disk_management_lists_persisted_records_and_scoped_clear_preserves_other_session() {
        let scratch = crate::scratch::Scratch::new("disk-memory-management");
        let project = scratch.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let root = scratch.path().join("memory-root");
        let general = scratch.path().join("home");
        let service = DiskMemoryManagement::open(&project, &root, &general).unwrap();
        for (name, session) in [("mine", "session-a"), ("theirs", "session-b")] {
            let input = parse_json(&format!(r#"{{"type":"project","scope":"project","name":"{name}","description":"{name}","summary":"{name}","profile":"{name}","capturedAt":"2026-10-28T00:00:00.000Z","sourceSessionKey":"{session}"}}"#)).unwrap();
            service
                .0
                .workspace_memory
                .upsert_candidate(input.as_object().unwrap())
                .unwrap();
        }
        assert_eq!(service.list(100).unwrap().len(), 2);
        assert_eq!(service.list(1).unwrap().len(), 1);
        service.clear_session(&"session-a".into()).unwrap();
        let remaining = service.list(100).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            remaining[0].get("sourceSessionKey"),
            Some(&JsValue::from("session-b"))
        );
        drop(service);
        let reopened = DiskMemoryManagement::open(&project, &root, &general).unwrap();
        assert_eq!(reopened.list(100).unwrap().len(), 1);
        reopened.clear("current_project").unwrap();
        assert!(reopened.list(100).unwrap().is_empty());
    }
}
