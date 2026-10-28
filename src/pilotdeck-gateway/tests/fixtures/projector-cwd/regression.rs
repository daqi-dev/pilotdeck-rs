//! #879: only bounded child workers change process cwd; the parent retains
//! scratch ownership, so this matrix serializes with the other cwd fixtures
//! without any in-process cwd lock.
use super::*;
use serde_json::{Value, json};
use std::path::Path;

const ORACLE: &str = include_str!("oracle.json");
const WORKER: &str = "runtime::turn::projector::cwd_tests::environment_worker";

fn matrix(name: &str) {
    crate::test_baseline::oracle(ORACLE);
    let scratch = crate::test_scratch::Scratch::new("projector-cwd");
    let root = fs::canonicalize(scratch.path()).expect("#879 setup canonical scratch");
    fs::create_dir(root.join("cwd")).expect("#879 setup cwd directory");
    fs::create_dir(root.join("tmp")).expect("#879 setup relative tmpdir");
    fs::create_dir(root.join("tmpdir")).expect("#879 setup absolute tmpdir");
    crate::test_process::run_cases(
        WORKER,
        vec![(
            name.into(),
            vec![
                ("PDRS_PROJECTOR_CWD_CASE".into(), name.into()),
                (
                    "PDRS_PROJECTOR_CWD_ROOT".into(),
                    root.to_str().expect("#879 setup UTF-8 root").into(),
                ),
            ],
        )],
    );
}

#[test]
#[ignore = "worker only; parent supplies isolated cwd and checks one receipt"]
fn environment_worker() {
    let name = std::env::var("PDRS_PROJECTOR_CWD_CASE").expect("#879 setup parent case");
    let root = std::env::var("PDRS_PROJECTOR_CWD_ROOT").expect("#879 setup parent scratch");
    let oracle: Value = serde_json::from_str(ORACLE).expect("#879 setup oracle JSON");
    let rows = oracle["cases"].as_array().expect("#879 setup oracle cases");
    let row = rows
        .iter()
        .find(|row| row["name"] == name)
        .expect("#879 setup oracle row");
    let cwd = Path::new(&root).join("cwd");
    let original_cwd = std::env::current_dir().expect("#879 setup original cwd");
    std::env::set_current_dir(&cwd).expect("#879 setup enter cwd");
    if name.contains("-warmed-") {
        // Rust has no process cwd cache; the read just proves the path was alive.
        std::env::current_dir().expect("#879 setup warm cwd");
    }
    if name.ends_with("-deleted") {
        fs::remove_dir(&cwd).expect("#879 setup remove cwd");
        assert!(
            std::env::current_dir().is_err(),
            "#879 setup deleted cwd must fail"
        );
    }
    let result = if name.starts_with("tmpdir-") {
        let absolute = name == "tmpdir-absolute-deleted";
        let tmp_root = format!("{root}/tmpdir");
        GatewayToolResultArtifactStore::with_default_root_from(
            move |key| match key {
                // A relative TMPDIR is resolved against process cwd, like Node
                // `resolve`; the absolute cell must never read cwd.
                "TMPDIR" => Some(std::ffi::OsString::from(if absolute {
                    tmp_root.clone()
                } else {
                    "../tmp".to_string()
                })),
                _ => None,
            },
            DEFAULT_ARTIFACT_THRESHOLD_BYTES,
        )
    } else {
        let input = if name.starts_with("absolute-") {
            format!("{root}//files/nested/../artifacts")
        } else {
            "../files/./nested/../artifacts".into()
        };
        GatewayToolResultArtifactStore::new(input, DEFAULT_ARTIFACT_THRESHOLD_BYTES)
    };
    // Restore before assertions, even if the product returned an unexpected result.
    std::env::set_current_dir(original_cwd).expect("#879 setup restore cwd");
    let actual = match result {
        Ok(store) => json!({
            "ok": true,
            "rootDir": store.root_dir().to_string_lossy(),
        }),
        Err(error) => json!({"error": {"message": error.to_string()}}),
    };
    let actual: Value = serde_json::from_str(&actual.to_string().replace(&root, "<root>"))
        .expect("#879 setup normalized JSON");
    let expected = if name.ends_with("-warmed-deleted") {
        // Registered Node warm-cwd-cache divergence: A6.9-PROJECTOR-CWD-001.
        // TS resolves against the cached (deleted) cwd; Rust keeps the cold
        // read's explicit error instead of a silent fallback.
        let cold_name = name.replace("-warmed", "");
        let cold = rows
            .iter()
            .find(|row| row["name"] == cold_name)
            .expect("#879 setup cold oracle row");
        assert_eq!(
            row["outcome"]["ok"],
            json!(true),
            "#879 setup warm TS success"
        );
        json!({"error": {"message": cold["outcome"]["error"]["message"]}})
    } else if row["outcome"].get("error").is_some() {
        json!({"error": {"message": row["outcome"]["error"]["message"]}})
    } else {
        json!({"ok": true, "rootDir": row["outcome"]["rootDir"]})
    };
    crate::negative_assert_eq!(actual, expected, "#879 projector cwd matrix: {name}");
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
