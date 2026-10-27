//! Dream review pure helpers (`dream-review.ts:73-531`).
//!
//! [`helpers`] ports trace/step/mutation construction, the two `localeCompare`
//! sorts, the LLM input projections, exclusive-cluster and General merge-group
//! validation, and the 120k-character user-note window. The runner itself
//! lives in the later `dream/runner.rs` / `dream/run.rs` work packages.
pub mod helpers;
pub mod runner;
