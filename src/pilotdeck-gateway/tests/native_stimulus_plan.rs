//! #1368 independent TS input declaration vs native collection plan.
use std::{path::PathBuf, process::Command};

fn controls(tests: &[&str]) {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/native-stimulus-plan/test_plan.py");
    let output = Command::new("python3")
        .arg("-I")
        .arg(script)
        .args(tests)
        .output()
        .expect("#1368 launch isolated stimulus plan controls");
    assert!(
        output.status.success(),
        "#1368 stimulus plan controls failed\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn native_stimulus_plan_controls() {
    controls(&[
        "PlanTests.test_boundary_coverage",
        "PlanTests.test_manifest_rejects_gap",
        "PlanTests.test_unknown_and_detached_plan",
        "PlanTests.test_unwitnessed_is_not_run",
    ]);
}

#[test]
#[ignore = "requires Node v22.22.0 / ICU 78.2; executes frozen TS preregistration"]
fn native_stimulus_plan_matches_preregistration() {
    controls(&["PlanTests.test_exact_registration"]);
}
