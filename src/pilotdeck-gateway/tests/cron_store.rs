//! #1216 cron storage replay guard. The per-cell unit replay lives in
//! `src/runtime/cron/tests.rs`; this target only carries the `#[ignore]`
//! regeneration guard that re-runs the generator against the real TS baseline
//! and requires a byte-identical oracle.json.
#[path = "common/baseline.rs"]
#[allow(dead_code)]
mod baseline;

/// Regenerates the oracle from the real TS baseline and compares bytes.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn cron_store_fixtures_regenerate_identically_from_ts() {
    baseline::regenerates_identically("cron-store");
}
