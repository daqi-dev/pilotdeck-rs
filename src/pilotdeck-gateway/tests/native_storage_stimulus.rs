#[allow(dead_code)]
#[path = "../src/process_group.rs"]
mod process_group;
#[allow(dead_code)]
// Module name matters: `src/test_process.rs` calls
// `crate::test_exact_receipt::single_pass`, and these integration crates
// `#[path]`-include that file, so each crate root must declare the same
// module name (the lib declares it from `tests/common/exact_receipt.rs`).
#[path = "common/exact_receipt.rs"]
mod test_exact_receipt;
#[allow(dead_code)]
#[path = "common/negative_receipt.rs"]
mod test_negative_receipt;
#[allow(dead_code)]
#[path = "common/permission_faults.rs"]
mod test_permission_faults;
#[allow(dead_code)]
#[path = "../src/test_process.rs"]
mod test_process;
#[path = "common/scratch.rs"]
mod test_scratch;
// Real binaries, isolated state, Node observer and local-only provider.
use std::{path::PathBuf, process::Command};
#[test]
#[ignore = "build gateway+sidecar bins first; requires Node v22.22.0 and the pinned TS baseline"]
fn executable_native_storage_stimulus() {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .expect("#1370 explicit CARGO_TARGET_DIR");
    let sidecar = target.join("debug/pilotdeck-agent-loop-sidecar");
    assert!(
        sidecar.is_file(),
        "#1370 build the real sidecar binary first"
    );
    let baseline = std::env::var_os("PDRS_BASELINE")
        .map(PathBuf::from)
        .expect("#1370 explicit read-only baseline");
    let output = std::env::var_os("PDRS_1370_PROCESS_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::fs::canonicalize(std::env::temp_dir())
                .expect("#1370 canonical temp evidence root")
                .join(format!("pdrs-1370-process-{}", uuid::Uuid::new_v4()))
        });
    assert!(!output.exists(), "#1370 fresh process evidence directory");
    let result = Command::new("node")
        .arg("--disable-warning=ExperimentalWarning")
        .arg("--import")
        .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/native-storage-stimulus/process.mts"),
        )
        .arg(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
        .arg(sidecar)
        .arg(&output)
        .output()
        .expect("#1370 launch storage stimulus observer");
    print!("{}", String::from_utf8_lossy(&result.stdout));
    eprint!("{}", String::from_utf8_lossy(&result.stderr));
    println!(
        "#1370 process evidence {} (caller owns cleanup)",
        output.display()
    );
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(output.join("process-record.json")).expect("#1370 process observer record"),
    )
    .expect("#1370 process observer JSON");
    let assertion = record["assertionFailure"].as_str();
    // The dedicated classified assertions must run before the generic nonzero
    // exit oracle: a mutation bound to one of them records its receipt and
    // panics here, while an unrelated earlier failure (timeout, another
    // scenario) only trips the unbound generic oracle and stays INVALID.
    negative_assert!(
        assertion != Some("#1370 unset keeps no stimulus receipt"),
        "#1370 executable unset stimulus inert"
    );
    negative_assert!(
        assertion != Some("#1370 unset turn completes"),
        "#1370 executable unset real turn"
    );
    negative_assert!(
        assertion != Some("#1370 unset transcript persists"),
        "#1370 executable unset real transcript"
    );
    negative_assert!(
        assertion != Some("#1370 stimulus receipt on stderr"),
        "#1370 executable armed stimulus receipt"
    );
    negative_assert!(
        assertion != Some("#1370 armed empty control turns normally"),
        "#1370 executable armed turn"
    );
    negative_assert!(
        assertion != Some("#1370 armed empty control publishes no receipts"),
        "#1370 executable armed no receipts"
    );
    negative_assert!(
        assertion != Some("#1370 invalid stimulus dir rejected"),
        "#1370 executable invalid stimulus dir"
    );
    negative_assert!(
        assertion != Some("#1370 invalid stimulus dir never advertises ready"),
        "#1370 executable invalid dir not ready"
    );
    negative_assert!(
        assertion != Some("#1370 replay stimulus receipt on stderr"),
        "#1370 executable replay stimulus receipt"
    );
    negative_assert!(
        assertion != Some("#1370 writer block receipt states the bound"),
        "#1370 executable writer block receipt"
    );
    negative_assert!(
        assertion != Some("#1370 transcript stays held before the release"),
        "#1370 executable writer holds the status write"
    );
    negative_assert!(
        assertion != Some("#1370 writer release lands the status write"),
        "#1370 executable writer release lands"
    );
    negative_assert!(
        assertion != Some("#1370 runs.json stays held before the release"),
        "#1370 executable registry holds the persist"
    );
    negative_assert!(
        assertion != Some("#1370 run_get held behind the registry barrier"),
        "#1370 executable run_get held"
    );
    negative_assert!(
        assertion != Some("#1370 run_get answers after the release"),
        "#1370 executable run_get after release"
    );
    negative_assert!(
        assertion != Some("#1370 runs.json lands the timed-out agent_status"),
        "#1370 executable registry release lands"
    );
    negative_assert!(
        assertion != Some("#1370 timeout turn final frame"),
        "#1370 executable timeout turn final frame"
    );
    negative_assert!(
        result.status.success(),
        "#1370 executable native storage stimulus oracle"
    );
}
