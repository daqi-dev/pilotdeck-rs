//! #2622 transcript-archive fixture guard. The per-cell replay of the oracle
//! rows through the native gateway lives in
//! `src/native_process/methods/transcript_archive.rs`; this target carries the
//! `#[ignore]` regeneration guard that re-runs the generator against the real
//! TS baseline and requires a byte-identical oracle.json.
#[path = "common/baseline.rs"]
#[allow(dead_code)]
mod baseline;

/// Regenerates the oracle from the real TS baseline and compares bytes.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn transcript_archive_fixtures_regenerate_identically_from_ts() {
    baseline::regenerates_identically("transcript-archive-2622");
}
