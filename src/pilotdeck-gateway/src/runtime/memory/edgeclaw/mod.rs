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
pub mod heartbeat;
pub mod llm;
pub mod messages;
mod node_fs;
pub mod provider;
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
