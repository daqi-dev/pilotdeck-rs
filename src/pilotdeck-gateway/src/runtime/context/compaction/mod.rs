//! Shared pure compaction with gateway-owned model and persistence ports.
pub mod engine;
pub mod orchestrator;
pub mod policy;
pub mod recovery;
pub use pilotdeck_context::*;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::HashSet;
