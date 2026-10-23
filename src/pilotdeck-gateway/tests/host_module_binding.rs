#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_protocol::JsValue;

/// Provenance: the committed handler-binding oracle must record the fixed TS
/// baseline and its source hashes. The byte-for-byte replay lives with the
/// ModuleRouter in host::tests::binding.
#[test]
fn handler_binding_oracle_records_ts_baseline() {
    let oracle = baseline::oracle(include_str!("fixtures/host-module-binding/oracle.json"));
    let cases = match oracle.get("cases") {
        Some(JsValue::Array(items)) => items,
        _ => panic!("oracle cases must be an array"),
    };
    assert_eq!(cases.len(), 27);
    let probes: usize = cases
        .iter()
        .map(|case| match case.get("probes") {
            Some(JsValue::Array(probes)) => probes.len(),
            _ => panic!("oracle case probes must be an array"),
        })
        .sum();
    assert_eq!(probes, 324, "27 cases x 12 probes");
}

/// The committed handler-binding oracle must regenerate byte-identically from
/// the real TS baseline.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn host_module_binding_fixtures_regenerate_identically_from_ts() {
    baseline::regenerates_identically("host-module-binding");
}
