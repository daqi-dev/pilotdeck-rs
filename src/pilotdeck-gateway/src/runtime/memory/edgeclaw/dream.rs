//! Dream review pure helpers (`dream-review.ts:73-531`).
//!
//! [`helpers`] ports trace/step/mutation construction, the two `localeCompare`
//! sorts, the LLM input projections, exclusive-cluster and General merge-group
//! validation, and the 120k-character user-note window. [`runner`] owns the
//! three sub-flows and [`run`] the complete staged orchestration.
pub mod helpers;
pub mod run;
pub mod runner;
