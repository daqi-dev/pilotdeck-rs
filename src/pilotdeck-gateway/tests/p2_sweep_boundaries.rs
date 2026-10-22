//! Live provenance/reproducibility checks for #415/#394/#244/#216.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules), Node 22 and mkfifo"]
fn boundary_fixtures_regenerate_twice_from_real_ts() {
    use std::{fs, path::Path, process::Command};
    let root = std::env::var_os("PDRS_BASELINE").expect("set PDRS_BASELINE");
    let loader = Path::new(&root).join("node_modules/tsx/dist/loader.mjs");
    let scratch = scratch::Scratch::new("p2-sweep-boundaries");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for name in [
        "provider-sync-abort",
        "mcp-worker-boundary",
        "mkdir-boundary",
        "run-registry-stale-race",
    ] {
        let fixture = fixtures.join(name);
        let committed = fs::read(fixture.join("oracle.json")).unwrap();
        baseline::oracle(std::str::from_utf8(&committed).unwrap());
        for round in 0..2 {
            let output = scratch.path().join(format!("{name}-{round}"));
            let result = Command::new("node")
                .arg("--import")
                .arg(&loader)
                .arg(fixture.join("generate.mts"))
                .arg(&output)
                .env("PDRS_BASELINE", &root)
                .output()
                .expect("run TS fixture generator");
            assert!(
                result.status.success(),
                "{name} round {round}: {}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(
                fs::read(output.join("oracle.json")).unwrap(),
                committed,
                "{name} round {round} differs from the real TS oracle"
            );
        }
        println!("{name}: real TS generated twice, both equal committed bytes");
    }
}
