#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_protocol::JsValue;

/// Provenance: the committed host-factory oracle records the fixed TS baseline
/// and the exact source hashes used by the probe.
#[test]
fn host_factory_oracle_records_ts_baseline() {
    let oracle = baseline::oracle(include_str!("fixtures/host-factory/oracle.json"));
    assert!(matches!(oracle.get("deployment"), Some(JsValue::Array(_))));
    assert!(matches!(oracle.get("lease"), Some(JsValue::Array(_))));
}

/// The committed oracle must regenerate byte-identically from the real TS
/// baseline (`generate.mts` writes with `flag: 'wx'` into a fresh directory).
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn host_factory_fixtures_regenerate_identically_from_ts() {
    baseline::regenerates_identically("host-factory");
}
