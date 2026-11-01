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
            // The native child canonicalizes --project/--home, but the
            // observer echoes the configured project path back as the
            // projectKey. On macOS the default temp dir sits behind the
            // /var -> /private/var symlink, so a raw temp_dir() root would
            // stop equaling its canonical self; hand the observer a
            // canonical root (the scratch-root precedent in
            // background_tasks.rs).
            std::fs::canonicalize(std::env::temp_dir())
                .expect("#1071 canonical temp evidence root")
                .join(format!("pdrs-1071-process-{}", uuid::Uuid::new_v4()))
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
        assertion != Some("#1131 readiness reports the kernel-assigned port"),
        "#1131 executable readiness reports the kernel-assigned port"
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

// #1261: anchor run()'s validate_environment call site, not the function body
// (production_entry_snapshot_capabilities_are_disabled already mirrors the
// combination by calling the function directly). Only the real --native child
// catches a removed or reordered call; the child is bounded so an accepted
// authority that starts listening still fails the deadline assertion instead
// of hanging the suite.
#[tokio::test]
async fn native_entry_process_rejects_inherited_authority() {
    let scratch = test_scratch::Scratch::new("1261-native-entry");
    let home = scratch.path().join("home");
    let project = scratch.path().join("project");
    std::fs::create_dir(&home).expect("#1261 native entry home directory");
    std::fs::create_dir(&project).expect("#1261 native entry project directory");
    let token_file = scratch.path().join("token");
    std::fs::write(&token_file, "1261-native-entry-token\n")
        .expect("#1261 native entry token file");
    let sidecar = scratch.path().join("sidecar");
    std::fs::write(&sidecar, "#1261 never executed before the rejection\n")
        .expect("#1261 native entry sidecar file");
    for key in [
        "PILOTDECK_STORAGE_ROOT",
        "PILOTDECK_SNAPSHOT_ROOT",
        "PILOTDECK_MANAGED_CONFIG",
    ] {
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
                .arg("--native")
                .arg("--home")
                .arg(&home)
                .arg("--project")
                .arg(&project)
                .arg("--listen")
                .arg("127.0.0.1:0")
                .arg("--token-file")
                .arg(&token_file)
                .arg("--sidecar")
                .arg(&sidecar)
                .env_clear()
                .env(key, "/1261-inherited/storage-root")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("#1261 native entry child rejection deadline")
        .expect("#1261 native entry child launch");
        let stderr =
            String::from_utf8(output.stderr).expect("#1261 native entry child stderr UTF8");
        negative_assert!(
            stderr.contains(&format!(
                "native isolated mode does not accept inherited {key}"
            )),
            "#1261 native entry rejects inherited {key}"
        );
        negative_assert!(
            !output.status.success(),
            "#1261 native entry rejects inherited {key}"
        );
    }
}
