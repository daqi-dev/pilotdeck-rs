//! #1328 weixin-channel fixture provenance: the committed `oracle.json`
//! must regenerate byte-identically from the fixed TS baseline. The per-case
//! replay lives in the library (`channel::weixin::replay_tests`); this guard
//! pins the generator side (U12 pr1 fixture, `weixin-channel/generate.mts`).
#[path = "common/baseline.rs"]
mod baseline;

/// The committed oracle's own shape: one declared case count, matching the
/// scenario definitions the replay iterates.
#[test]
fn weixin_channel_committed_oracle_shape() {
    let oracle = baseline::oracle(include_str!("fixtures/weixin-channel/oracle.json"));
    let case_count = match oracle.get("cases").expect("#1328 oracle cases") {
        pilotdeck_protocol::JsValue::Array(cases) => cases.len(),
        _ => panic!("#1328 oracle cases must be an array"),
    };
    assert_eq!(case_count, 11, "#1328 weixin oracle case count");
    let declared = oracle
        .get("expectedCaseCount")
        .and_then(|count| count.as_number())
        .expect("#1328 oracle expectedCaseCount");
    assert_eq!(
        case_count as f64, declared,
        "#1328 oracle declares its case count"
    );
}

/// Re-runs the real TS generator and requires a byte-identical oracle.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules) and Node v22.22.0"]
fn weixin_channel_fixtures_regenerate_identically_from_ts() {
    baseline::regenerates_identically("weixin-channel");
}
