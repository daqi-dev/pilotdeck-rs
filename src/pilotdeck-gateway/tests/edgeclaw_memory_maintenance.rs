#[path = "common/baseline.rs"]
mod baseline;
#[test]
#[ignore = "requires fixed TS checkout and Node v22.22.0"]
fn real_ts_regenerates_identically() {
    baseline::oracle(include_str!("fixtures/memory-maintenance/oracle.json"));
    baseline::regenerates_identically("memory-maintenance");
}
