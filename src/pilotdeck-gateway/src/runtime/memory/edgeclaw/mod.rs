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
pub mod dream;
pub mod file_memory;
pub mod model;
pub mod heartbeat;
pub mod llm;
pub mod messages;
mod node_fs;
pub mod provider;
pub mod runtime_surface;
#[path = "retrieval/helpers.rs"]
pub mod retrieval;
#[path = "retrieval/retriever.rs"]
pub mod retriever;
pub mod service;
pub mod storage;
pub mod text;

pub use file_memory::{
    EdgeClawFileStores, FileMemoryError, FileMemoryStore, FileMemoryStoreOptions,
};

pub use storage::{EdgeClawStorage, IndexingSettings, L0SessionRecord, StorageError};
pub use model::native_llm_from_snapshot;
pub use runtime_surface::{
    NativeEdgeClawClock, NativeEdgeClawLlm, NativeEdgeClawLlmFuture, NativeEdgeClawOptions,
    NativeEdgeClawRuntime, stage_native_from_snapshot,
};
pub use runtime_surface::read_workspace_dir_from_data_dir;

pub mod factory;
mod owner;
pub use owner::{NativeProjectMemoryOwner, create_project_owner};
