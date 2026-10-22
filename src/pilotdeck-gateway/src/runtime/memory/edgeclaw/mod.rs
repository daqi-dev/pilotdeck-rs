//! EdgeClaw memory storage layer (SQLite).
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
//! File memory stores, snapshots, dream stage root swaps, retrieval,
//! extraction and heartbeat are deliberately out of scope; see the delivery
//! note for the follow-up split.
pub mod storage;

pub use storage::{EdgeClawStorage, IndexingSettings, L0SessionRecord, StorageError};
