//! #879: only bounded child workers change process cwd; the parent retains
//! scratch ownership, so this matrix serializes with the other cwd fixtures
//! without any in-process cwd lock.
//!
//! This is a normal unit-test module under `runtime::turn`, not a `#[path]`
//! fixture attached to `projector.rs`: the agent-event-projector negative
//! control copies `projector.rs` + `projector_tests.rs` into a scratch crate
//! without `tests/fixtures`, and the copied source must stay self-contained.

use crate::runtime::turn::projector::{
    DEFAULT_ARTIFACT_THRESHOLD_BYTES, GatewayToolResultArtifactStore, node_temp_dir,
};
use crate::test_negative_receipt::expect_err;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

const ORACLE: &str = include_str!("../../../tests/fixtures/projector-cwd/oracle.json");
const WORKER: &str = "runtime::turn::projector_cwd_tests::environment_worker";

fn matrix(name: &str) {
    crate::test_baseline::oracle(ORACLE);
    // Environment preparation must never write the target nonce receipt: a
    // permission/IO failure here is INVALID for a negative control, not a kill.
    let scratch = crate::test_scratch::Scratch::new("projector-cwd");
    let root =
        fs::canonicalize(scratch.path()).expect("#879 projector cwd setup: canonicalize scratch");
    fs::create_dir(root.join("cwd")).expect("#879 projector cwd setup: create cwd");
    fs::create_dir(root.join("tmp")).expect("#879 projector cwd setup: create tmp");
    fs::create_dir(root.join("tmpdir")).expect("#879 projector cwd setup: create tmpdir");
    let mut env = vec![
        ("PDRS_PROJECTOR_CWD_CASE".into(), name.into()),
        (
            "PDRS_PROJECTOR_CWD_ROOT".into(),
            root.to_str()
                .expect("#879 projector cwd setup: UTF-8 root")
                .into(),
        ),
    ];
    if name.starts_with("tmpdir-") {
        // A relative TMPDIR is resolved against process cwd, like Node
        // `resolve`; the absolute cell must never read cwd.
        env.push((
            "TMPDIR".into(),
            if name == "tmpdir-absolute-deleted" {
                format!("{}/tmpdir", root.display())
            } else {
                "../tmp".into()
            },
        ));
    }
    crate::test_process::run_cases(WORKER, vec![(name.into(), env)]);
}

#[test]
#[ignore = "worker only; parent supplies isolated cwd and checks one receipt"]
fn environment_worker() {
    // Worker setup and restoration stay off the receipt protocol: only a
    // product result below may produce the target `#879 projector cwd matrix`
    // nonce. An environment failure therefore leaves a named FAILED test with
    // no receipt, which every judge classifies as INVALID.
    let name =
        std::env::var("PDRS_PROJECTOR_CWD_CASE").expect("#879 projector cwd setup: case env");
    let assertion = format!("#879 projector cwd matrix: {name}");
    let root =
        std::env::var("PDRS_PROJECTOR_CWD_ROOT").expect("#879 projector cwd setup: root env");
    let oracle: Value =
        serde_json::from_str(ORACLE).expect("#879 projector cwd setup: oracle JSON");
    let rows = oracle["cases"]
        .as_array()
        .expect("#879 projector cwd setup: oracle cases");
    let row = rows
        .iter()
        .find(|row| row["name"] == name)
        .expect("#879 projector cwd setup: oracle case row");
    let cwd = Path::new(&root).join("cwd");
    let original_cwd =
        std::env::current_dir().expect("#879 projector cwd setup: read original cwd");
    std::env::set_current_dir(&cwd).expect("#879 projector cwd setup: enter case cwd");
    if name.contains("-warmed-") {
        // Rust has no process cwd cache; the read just proves the path was alive.
        std::env::current_dir().expect("#879 projector cwd setup: warm cwd read");
    }
    if name.ends_with("-deleted") {
        fs::remove_dir(&cwd).expect("#879 projector cwd setup: delete case cwd");
        assert!(
            std::env::current_dir().is_err(),
            "#879 projector cwd setup: deleted cwd must be unreadable"
        );
    }
    if name == "tmpdir-relative-deleted" || name == "tmpdir-relative-warmed-deleted" {
        // `with_default_root` reads the relative TMPDIR first and then `build`
        // re-reads cwd for the relative root, so a silent `node_temp_dir`
        // fallback would be masked by the second read. Probe the first read
        // directly and bind the same matrix assertion to its failure.
        let error = expect_err(node_temp_dir(|key| std::env::var_os(key)), &assertion);
        crate::negative_assert_eq!(
            error.to_string(),
            "ENOENT: no such file or directory, uv_cwd",
            "{assertion}"
        );
    }
    let result = if name.starts_with("tmpdir-") {
        GatewayToolResultArtifactStore::with_default_root()
    } else {
        let input = if name.starts_with("absolute-") {
            format!("{root}//files/nested/../artifacts")
        } else {
            "../files/./nested/../artifacts".into()
        };
        GatewayToolResultArtifactStore::new(input, DEFAULT_ARTIFACT_THRESHOLD_BYTES)
    };
    // Restore before assertions, even if the product returned an unexpected result.
    std::env::set_current_dir(original_cwd)
        .expect("#879 projector cwd setup: restore original cwd");
    let actual = match result {
        Ok(store) => json!({
            "ok": true,
            "rootDir": store.root_dir().to_string_lossy(),
        }),
        Err(error) => json!({"error": {"message": error.to_string()}}),
    };
    let actual: Value = serde_json::from_str(&actual.to_string().replace(&root, "<root>"))
        .expect("#879 projector cwd setup: product JSON");
    let expected = if name.ends_with("-warmed-deleted") {
        // Registered Node warm-cwd-cache divergence: A6.9-PROJECTOR-CWD-001.
        // TS resolves against the cached (deleted) cwd; Rust keeps the cold
        // read's explicit error instead of a silent fallback.
        let cold_name = name.replace("-warmed", "");
        let cold = rows
            .iter()
            .find(|row| row["name"] == cold_name)
            .expect("#879 projector cwd setup: cold oracle row");
        assert_eq!(
            row["outcome"]["ok"],
            json!(true),
            "#879 projector cwd setup: warm-deleted oracle row"
        );
        json!({"error": {"message": cold["outcome"]["error"]["message"]
            .as_str()
            .expect("#879 projector cwd setup: cold oracle error")}})
    } else if row["outcome"].get("error").is_some() {
        json!({"error": {"message": row["outcome"]["error"]["message"]
            .as_str()
            .expect("#879 projector cwd setup: oracle error message")}})
    } else {
        json!({"ok": true, "rootDir": row["outcome"]["rootDir"]
            .as_str()
            .expect("#879 projector cwd setup: oracle rootDir")})
    };
    crate::negative_assert_eq!(actual, expected, "{assertion}");
    crate::test_process::receipt(&name);
}

macro_rules! case {
    ($test:ident, $name:literal) => {
        #[test]
        fn $test() {
            matrix($name);
        }
    };
}
case!(relative_normal_cwd, "relative-normal");
case!(relative_deleted_cwd, "relative-deleted");
case!(relative_warmed_normal_cwd, "relative-warmed-normal");
case!(relative_warmed_deleted_cwd, "relative-warmed-deleted");
case!(absolute_normal_cwd, "absolute-normal");
case!(absolute_deleted_cwd, "absolute-deleted");
case!(tmpdir_relative_normal_cwd, "tmpdir-relative-normal");
case!(tmpdir_relative_deleted_cwd, "tmpdir-relative-deleted");
case!(
    tmpdir_relative_warmed_deleted_cwd,
    "tmpdir-relative-warmed-deleted"
);
case!(tmpdir_absolute_deleted_cwd, "tmpdir-absolute-deleted");

/// A failed environment step must stay INVALID for the negative-control judge:
/// the worker is named FAILED but writes no target nonce. Root exists without
/// its `cwd` child, so entering the case directory fails before any product
/// call; the receipt configuration targets the same test/assertion the matrix
/// controls use. If a setup step is ever bound back to the matrix assertion,
/// this test observes the receipt and fails.
#[test]
fn environment_failure_is_not_a_bound_negative_receipt() {
    let scratch = crate::test_scratch::Scratch::new("projector-cwd-setup");
    let receipt = scratch.path().join("setup.receipt");
    let config = json!({
        "nonce": "8790".repeat(16),
        "targets": [{
            "test": WORKER,
            "assertion": "#879 projector cwd matrix: relative-normal",
            "path": receipt
                .to_str()
                .expect("#879 projector cwd setup regression: receipt path"),
        }],
    });
    let output = std::process::Command::new(
        std::env::current_exe().expect("#879 projector cwd setup regression: test executable"),
    )
    .args([
        "--exact",
        WORKER,
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ])
    .env_clear()
    .env("PDRS_PROJECTOR_CWD_CASE", "relative-normal")
    .env("PDRS_PROJECTOR_CWD_ROOT", scratch.path())
    .env(
        crate::test_negative_receipt::CONFIG_ENV,
        serde_json::to_string(&config).expect("#879 projector cwd setup regression: config JSON"),
    )
    .output()
    .expect("#879 projector cwd setup regression: child result");
    let stdout = String::from_utf8(output.stdout)
        .expect("#879 projector cwd setup regression: child stdout UTF-8");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "#879 projector cwd setup regression: environment failure must fail the worker: {stdout}\n{stderr}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == format!("test {WORKER} ... FAILED")),
        "#879 projector cwd setup regression: named FAILED required: {stdout}\n{stderr}"
    );
    // The bound assertion must be reachable before the message-shape check:
    // a setup step rebound to the matrix assertion fails here first.
    crate::negative_assert!(
        !receipt.exists(),
        "#879 projector cwd environment: target receipt must be absent"
    );
    assert!(
        stderr.contains("#879 projector cwd setup:"),
        "#879 projector cwd setup regression: failure must be a setup step: {stdout}\n{stderr}"
    );
}

#[test]
#[ignore = "requires Node v22.22.0 and PDRS_BASELINE"]
fn projector_cwd_regenerates_identically() {
    crate::test_baseline::regenerates_identically("projector-cwd");
}
