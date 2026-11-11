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
#[allow(dead_code)]
#[path = "common/skip_line.rs"]
mod test_skip_line;
// #1371 real binaries, isolated state, Node observer and local-only provider.
use std::{path::PathBuf, process::Command};
#[test]
#[ignore = "build gateway+sidecar bins first; requires Node v22.22.0 and the pinned TS baseline"]
fn executable_native_loop_stimulus() {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .expect("#1371 explicit CARGO_TARGET_DIR");
    let sidecar = target.join("debug/pilotdeck-agent-loop-sidecar");
    assert!(
        sidecar.is_file(),
        "#1371 build the real sidecar binary first"
    );
    let baseline = std::env::var_os("PDRS_BASELINE")
        .map(PathBuf::from)
        .expect("#1371 explicit read-only baseline");
    let output = std::env::var_os("PDRS_1371_PROCESS_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::fs::canonicalize(std::env::temp_dir())
                .expect("#1371 canonical temp evidence root")
                .join(format!("pdrs-1371-process-{}", uuid::Uuid::new_v4()))
        });
    assert!(!output.exists(), "#1371 fresh process evidence directory");
    let result = Command::new("node")
        .arg("--disable-warning=ExperimentalWarning")
        .arg("--import")
        .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/native-loop-stimulus/process.mts"),
        )
        .arg(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
        .arg(sidecar)
        .arg(&output)
        .output()
        .expect("#1371 launch loop stimulus observer");
    print!("{}", String::from_utf8_lossy(&result.stdout));
    eprint!("{}", String::from_utf8_lossy(&result.stderr));
    println!(
        "#1371 process evidence {} (caller owns cleanup)",
        output.display()
    );
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(output.join("process-record.json")).expect("#1371 process observer record"),
    )
    .expect("#1371 process observer JSON");
    let assertion = record["assertionFailure"].as_str();
    // The dedicated classified assertions must run before the generic nonzero
    // exit oracle: a mutation bound to one of them records its receipt and
    // panics here, while an unrelated earlier failure only trips the unbound
    // generic oracle and stays INVALID.
    for (message, label) in [
        (
            "#1371 unset keeps no stimulus receipt",
            "#1371 executable unset stimulus inert",
        ),
        (
            "#1371 unset turn completes",
            "#1371 executable unset real turn",
        ),
        (
            "#1371 unset real model text",
            "#1371 executable unset real text",
        ),
        (
            "#1371 unset keeps the title request",
            "#1371 executable unset title request",
        ),
        (
            "#1371 stimulus receipt on stderr",
            "#1371 executable armed receipt",
        ),
        (
            "#1371 armed inert turn completes",
            "#1371 executable armed inert turn",
        ),
        (
            "#1371 inert turn real model text",
            "#1371 executable inert real text",
        ),
        (
            "#1371 inert turn keeps the title request",
            "#1371 executable inert title request",
        ),
        (
            "#1371 armed inert publishes no receipts",
            "#1371 executable armed inert quiet",
        ),
        (
            "#1371 invalid stimulus dir rejected",
            "#1371 executable invalid dir rejected",
        ),
        (
            "#1371 invalid stimulus dir reports the gate",
            "#1371 executable invalid dir gate",
        ),
        (
            "#1371 invalid stimulus dir never advertises ready",
            "#1371 executable invalid dir not ready",
        ),
        (
            "#1371 tool phase model prefix",
            "#1371 executable tool model prefix",
        ),
        (
            "#1371 tool phase controlled_wait prefix",
            "#1371 executable tool controlled_wait prefix",
        ),
        (
            "#1371 tool phase unknown terminal",
            "#1371 executable tool unknown terminal",
        ),
        (
            "#1371 tool phase no timeout",
            "#1371 executable tool no timeout",
        ),
        (
            "#1371 armed loop suppresses the automatic title request",
            "#1371 executable armed loop no title request",
        ),
        (
            "#1371 timeout-first ends red",
            "#1371 executable timeout-first red",
        ),
        (
            "#1371 timeout-first stays bounded",
            "#1371 executable timeout-first bounded",
        ),
        (
            "#1371 timeout-first outer fence current",
            "#1371 executable timeout-first fence current",
        ),
        (
            "#1371 timeout-first late unknown is not a success",
            "#1371 executable timeout-first late unknown",
        ),
        (
            "#1371 unknown-first terminal is an error",
            "#1371 executable unknown-first error terminal",
        ),
        (
            "#1371 unknown-first precedes the timer",
            "#1371 executable unknown-first before timer",
        ),
        (
            "#1371 unknown-first timer cleared",
            "#1371 executable unknown-first timer cleared",
        ),
        (
            "#1371 known-final-first completes",
            "#1371 executable known-first completes",
        ),
        (
            "#1371 known-final-first timer cleared",
            "#1371 executable known-first timer cleared",
        ),
        (
            "#1371 after-complete ends red",
            "#1371 executable after-complete red",
        ),
        (
            "#1371 after-complete fence completes before the late unknown",
            "#1371 executable after-complete fence",
        ),
        (
            "#1371 return-pending completed stays buffered",
            "#1371 executable return-pending buffered",
        ),
        (
            "#1371 return-pending completed is not published before the release",
            "#1371 executable return-pending early final",
        ),
        (
            "#1371 no fence complete receipt for key loop-after-complete-run",
            "#1371 executable after-complete fence missing",
        ),
        (
            "#1371 return-pending ends red",
            "#1371 executable return-pending red",
        ),
        (
            "#1371 return-pending released inside the bound",
            "#1371 executable return-pending released",
        ),
        (
            "#1371 return-pending late release is not a live success",
            "#1371 executable return-pending late success",
        ),
        (
            "#1371 bound timeout still ends red",
            "#1371 executable bound timeout",
        ),
        (
            "#1371 bound expires instead of hanging",
            "#1371 executable bound expires",
        ),
        (
            "#1371 bound never fabricates a completion",
            "#1371 executable bound no completion",
        ),
        (
            "#1371 provider-armed loop reaches real model",
            "#1371 executable provider arm invokes real model",
        ),
        (
            "#1371 provider completion precedes the wait",
            "#1371 executable provider completion precedes wait",
        ),
        (
            "#1371 provider-armed known outcome completes",
            "#1371 executable provider armed outcome completes",
        ),
    ] {
        negative_assert!(assertion != Some(message), "{}", label);
    }
    negative_assert!(
        result.status.success(),
        "#1371 executable native loop stimulus oracle"
    );
}
