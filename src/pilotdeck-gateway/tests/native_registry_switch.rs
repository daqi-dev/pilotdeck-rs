#[allow(dead_code)]
#[path = "common/negative_receipt.rs"]
mod test_negative_receipt;
#[allow(dead_code)]
#[path = "../src/test_process.rs"]
mod test_process;
#[path = "common/scratch.rs"]
mod test_scratch;
// Real binaries, isolated state, Node observer and local-only provider.
use std::{path::PathBuf, process::Command};
#[test]
#[ignore = "build gateway+sidecar bins first; requires Node v22.22.0 and the pinned TS baseline"]
fn executable_native_registry_switch() {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .expect("#1369 explicit CARGO_TARGET_DIR");
    let sidecar = target.join("debug/pilotdeck-agent-loop-sidecar");
    assert!(
        sidecar.is_file(),
        "#1369 build the real sidecar binary first"
    );
    let baseline = std::env::var_os("PDRS_BASELINE")
        .map(PathBuf::from)
        .expect("#1369 explicit read-only baseline");
    // The native child canonicalizes --project/--home, but the observer
    // echoes the configured project path back as the projectKey. Hand the
    // observer a canonical root (the scratch-root precedent in
    // background_tasks.rs) so a symlinked temp dir cannot break equality.
    let output = std::env::var_os("PDRS_1369_PROCESS_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::fs::canonicalize(std::env::temp_dir())
                .expect("#1369 canonical temp evidence root")
                .join(format!("pdrs-1369-process-{}", uuid::Uuid::new_v4()))
        });
    assert!(!output.exists(), "#1369 fresh process evidence directory");
    let result = Command::new("node")
        .arg("--disable-warning=ExperimentalWarning")
        .arg("--import")
        .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/native-registry-switch/process.mts"),
        )
        .arg(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
        .arg(sidecar)
        .arg(&output)
        .output()
        .expect("#1369 launch registry switch observer");
    print!("{}", String::from_utf8_lossy(&result.stdout));
    eprint!("{}", String::from_utf8_lossy(&result.stderr));
    println!(
        "#1369 process evidence {} (caller owns cleanup)",
        output.display()
    );
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(output.join("process-record.json")).expect("#1369 process observer record"),
    )
    .expect("#1369 process observer JSON");
    let assertion = record["assertionFailure"].as_str();
    // The dedicated classified assertions must run before the generic nonzero
    // exit oracle: a mutation bound to one of them records its receipt and
    // panics here, while an unrelated earlier failure (timeout, another
    // scenario) only trips the unbound generic oracle and stays INVALID.
    negative_assert!(
        assertion != Some("#1369 off registry receipt on stderr"),
        "#1369 executable off registry receipt"
    );
    negative_assert!(
        assertion != Some("#1369 on registry receipt on stderr"),
        "#1369 executable on registry receipt"
    );
    negative_assert!(
        assertion != Some("#1369 describe_server gates run capabilities"),
        "#1369 executable describe_server registry gating"
    );
    negative_assert!(
        assertion != Some("#1369 off leaves no runs.json"),
        "#1369 executable off run registry absent"
    );
    negative_assert!(
        assertion != Some("#1369 on run_get reads the registry"),
        "#1369 executable on run registry read"
    );
    negative_assert!(
        assertion != Some("#1369 on run_get terminal state"),
        "#1369 executable on persisted run terminal"
    );
    negative_assert!(
        assertion != Some("#1369 off run_get unavailable code"),
        "#1369 executable off run read unavailable"
    );
    negative_assert!(
        assertion != Some("#1369 off turn completes"),
        "#1369 executable off real turn"
    );
    negative_assert!(
        assertion != Some("#1369 off transcript persists"),
        "#1369 executable off real transcript"
    );
    negative_assert!(
        assertion != Some("#1369 invalid registry switch rejected"),
        "#1369 executable invalid registry switch"
    );
    negative_assert!(
        result.status.success(),
        "#1369 executable native registry switch oracle"
    );
}
