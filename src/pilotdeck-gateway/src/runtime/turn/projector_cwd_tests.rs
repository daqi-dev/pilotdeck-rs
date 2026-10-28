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
use crate::test_negative_receipt::{expect_err, expect_ok, expect_some};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

const ORACLE: &str = include_str!("../../../tests/fixtures/projector-cwd/oracle.json");
const WORKER: &str = "runtime::turn::projector_cwd_tests::environment_worker";

fn matrix(name: &str) {
    let assertion = format!("#879 projector cwd matrix: {name}");
    crate::test_baseline::oracle(ORACLE);
    let scratch = crate::test_scratch::Scratch::new("projector-cwd");
    let root = expect_ok(fs::canonicalize(scratch.path()), &assertion);
    expect_ok(fs::create_dir(root.join("cwd")), &assertion);
    expect_ok(fs::create_dir(root.join("tmp")), &assertion);
    expect_ok(fs::create_dir(root.join("tmpdir")), &assertion);
    let mut env = vec![
        ("PDRS_PROJECTOR_CWD_CASE".into(), name.into()),
        (
            "PDRS_PROJECTOR_CWD_ROOT".into(),
            expect_some(root.to_str(), &assertion).into(),
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
    let name = expect_ok(
        std::env::var("PDRS_PROJECTOR_CWD_CASE"),
        "#879 projector cwd matrix: missing case",
    );
    let assertion = format!("#879 projector cwd matrix: {name}");
    let root = expect_ok(std::env::var("PDRS_PROJECTOR_CWD_ROOT"), &assertion);
    let oracle: Value = expect_ok(serde_json::from_str(ORACLE), &assertion);
    let rows = expect_some(oracle["cases"].as_array(), &assertion);
    let row = expect_some(rows.iter().find(|row| row["name"] == name), &assertion);
    let cwd = Path::new(&root).join("cwd");
    let original_cwd = expect_ok(std::env::current_dir(), &assertion);
    expect_ok(std::env::set_current_dir(&cwd), &assertion);
    if name.contains("-warmed-") {
        // Rust has no process cwd cache; the read just proves the path was alive.
        expect_ok(std::env::current_dir(), &assertion);
    }
    if name.ends_with("-deleted") {
        expect_ok(fs::remove_dir(&cwd), &assertion);
        crate::negative_assert!(std::env::current_dir().is_err(), "{assertion}");
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
    expect_ok(std::env::set_current_dir(original_cwd), &assertion);
    let actual = match result {
        Ok(store) => json!({
            "ok": true,
            "rootDir": store.root_dir().to_string_lossy(),
        }),
        Err(error) => json!({"error": {"message": error.to_string()}}),
    };
    let actual: Value = expect_ok(
        serde_json::from_str(&actual.to_string().replace(&root, "<root>")),
        &assertion,
    );
    let expected = if name.ends_with("-warmed-deleted") {
        // Registered Node warm-cwd-cache divergence: A6.9-PROJECTOR-CWD-001.
        // TS resolves against the cached (deleted) cwd; Rust keeps the cold
        // read's explicit error instead of a silent fallback.
        let cold_name = name.replace("-warmed", "");
        let cold = expect_some(rows.iter().find(|row| row["name"] == cold_name), &assertion);
        crate::negative_assert_eq!(row["outcome"]["ok"], json!(true), "{assertion}");
        json!({"error": {"message": cold["outcome"]["error"]["message"]}})
    } else if row["outcome"].get("error").is_some() {
        json!({"error": {"message": row["outcome"]["error"]["message"]}})
    } else {
        json!({"ok": true, "rootDir": row["outcome"]["rootDir"]})
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

#[test]
#[ignore = "requires Node v22.22.0 and PDRS_BASELINE"]
fn projector_cwd_regenerates_identically() {
    crate::test_baseline::regenerates_identically("projector-cwd");
}
