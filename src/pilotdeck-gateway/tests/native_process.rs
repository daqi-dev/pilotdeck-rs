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
fn executable_native_and_a4() {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .expect("#1071 explicit CARGO_TARGET_DIR");
    let sidecar = target.join("debug/pilotdeck-agent-loop-sidecar");
    assert!(
        sidecar.is_file(),
        "#1071 build the real sidecar binary first"
    );
    let baseline = std::env::var_os("PDRS_BASELINE")
        .map(PathBuf::from)
        .expect("#1071 explicit read-only baseline");
    let output = std::env::var_os("PDRS_1071_PROCESS_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("pdrs-1071-process-{}", uuid::Uuid::new_v4()))
        });
    assert!(!output.exists(), "#1071 fresh process evidence directory");
    let result = Command::new("node")
        .arg("--disable-warning=ExperimentalWarning")
        .arg("--import")
        .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/native-process/process.mts"),
        )
        .arg(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
        .arg(sidecar)
        .arg(&output)
        .output()
        .expect("#1071 launch process observer");
    print!("{}", String::from_utf8_lossy(&result.stdout));
    eprint!("{}", String::from_utf8_lossy(&result.stderr));
    println!(
        "#1071 process evidence {} (caller owns cleanup)",
        output.display()
    );
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(output.join("process-record.json")).expect("#1071 process observer record"),
    )
    .expect("#1071 process observer JSON");
    let assertion = record["assertionFailure"].as_str();
    // The dedicated classified assertions must run before the generic nonzero
    // exit oracle: a mutation bound to one of them records its receipt and
    // panics here, while an unrelated earlier failure (timeout, another
    // scenario) only trips the unbound generic oracle and stays INVALID.
    negative_assert!(
        assertion != Some("#1071 clean signal exit"),
        "#1071 executable clean signal exit"
    );
    negative_assert!(
        assertion != Some("#1071 refreshed hooks refused"),
        "#1071 executable refreshed hooks refused"
    );
    negative_assert!(
        assertion != Some("#1071 A4 bind collision exhausted"),
        "#1071 executable A4 bind collision exhausted"
    );
    negative_assert!(
        assertion != Some("#1071 readiness address carries a port"),
        "#1071 executable readiness address carries a port"
    );
    negative_assert!(
        result.status.success(),
        "#1071 executable native and A4 oracle"
    );
}

#[tokio::test]
async fn explicit_entry_selects_parser() {
    for (args, native) in [
        (vec!["--native", "--probe-invalid"], true),
        (vec!["a4", "--native", "--probe-invalid"], false),
        (vec!["--native=true", "--probe-invalid"], false),
    ] {
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
                .args(&args)
                .env_clear()
                .env("PILOTDECK_PROXY_LISTEN", "invalid-a4-address")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("#1071 entry parser process deadline")
        .expect("#1071 entry parser process launch");
        assert!(
            !output.status.success(),
            "#1071 invalid parser input exits nonzero"
        );
        let error = String::from_utf8(output.stderr).expect("#1071 entry stderr UTF8");
        println!("#1071 entry args={args:?}: {error}");
        if native {
            negative_assert!(
                error.contains("unknown native option: --probe-invalid"),
                "#1071 native first argument selects native parser"
            );
        } else {
            negative_assert!(
                !error.contains("native option") && error.contains("AddrParseError(Socket)"),
                "#1071 nonfirst native argument keeps A4 parser"
            );
        }
    }
}
