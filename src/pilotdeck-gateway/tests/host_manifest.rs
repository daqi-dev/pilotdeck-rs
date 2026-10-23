#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_protocol::JsValue;

/// Provenance: the committed manifest oracle must record the fixed TS
/// baseline and its source hashes. The byte-for-byte replay lives with the
/// pure implementation in host::modules::manifest.
#[test]
fn manifest_oracle_records_ts_baseline() {
    let oracle = baseline::oracle(include_str!("fixtures/host-manifest/oracle.json"));
    let cases = match oracle.get("cases") {
        Some(JsValue::Array(items)) => items.len(),
        _ => panic!("oracle cases must be an array"),
    };
    assert_eq!(cases, 48);
}

/// The committed manifest oracle must regenerate byte-identically from the
/// real TS baseline.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn host_manifest_fixtures_regenerate_identically_from_ts() {
    baseline::regenerates_identically("host-manifest");
}
