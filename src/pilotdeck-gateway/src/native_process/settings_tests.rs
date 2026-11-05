//! #1320: replay of the real TS native-settings oracle
//! (`tests/fixtures/native-settings/generate.mts`). The fixture records the
//! six U7 settings methods over the same layers the native request route
//! serves — a real WS connection shape (the response frame plus every
//! notification received before/after it), a follow-up `resolve_settings`
//! re-read proving read/write share one store generation, and the post
//! state. The replay lands here with the S4 segment; this file first ships
//! the regeneration guard that keeps the committed oracle byte-identical to
//! the TS baseline.
use crate::test_baseline;

/// Regeneration guard for the TS golden; run once with `PDRS_BASELINE` set.
#[test]
#[ignore = "regenerates the TS fixture; needs PDRS_BASELINE and Node"]
fn native_settings_fixture_regenerates_identically_from_ts() {
    test_baseline::regenerates_identically("native-settings");
}
