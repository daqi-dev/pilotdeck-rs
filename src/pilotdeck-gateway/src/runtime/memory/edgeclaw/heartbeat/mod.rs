//! Heartbeat indexing pipeline (A6.6-edgeclaw-12).
//!
//! [`helpers`] ports the pure functions of the fixed TS
//! `core/pipeline/heartbeat.ts:22-446`: duplicate detection, CJK search
//! tokenization, General-project shortlisting, candidate routing text/preview,
//! batch flatten/merge, focus-turn derivation, and the index trace/step/detail
//! constructors. [`indexer`] adds the stateful `HeartbeatIndexer` pipeline on
//! top of them (A6.6-edgeclaw-13).

pub mod helpers;
pub mod indexer;
