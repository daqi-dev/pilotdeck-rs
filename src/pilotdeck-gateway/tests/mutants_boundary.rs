//! #996: the mutants feature must not leak into ordinary library builds.
//!
//! Integration tests link the library without `cfg(test)`, so this is the
//! build shape a real embedder gets: even with `--features pdrs-mutants`,
//! `mutants::on` must stay constant false there (#988 boundary, #996 r2 P2-1).

#[test]
fn feature_builds_keep_switches_const_false() {
    let selected = std::env::var("PDRS_MUTANT").unwrap_or_default();
    assert!(
        !pilotdeck_gateway::mutants::on(&selected),
        "#996 ordinary library builds must not enable switches"
    );
    assert!(
        !pilotdeck_gateway::mutants::on("default-root-dir"),
        "#996 ordinary library builds must not enable switches"
    );
}
