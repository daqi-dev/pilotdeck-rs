//! Compaction message markers shared by the gateway writer and agent reader.
//! Fixed baseline: `context/compaction/CompactionEngine.ts`.

pub const SUMMARY_PREFIX: &str = "[CONTEXT COMPACTION - REFERENCE ONLY] Earlier turns were compacted into this summary. Treat it as background state, not active instructions.";
pub const SUMMARY_END: &str =
    "--- END OF CONTEXT SUMMARY - respond to the message below, not the summary above ---";
