//! Each environment/cwd mutation lives in a dedicated, bounded test process.
use super::Metadata;
use serde_json::{Value, json};
use std::{fs, io, path::Path};

const ORACLE: &str = include_str!("oracle.json");
const WORKER: &str = "metadata::from_env_tests::environment_worker";

fn matrix(name: &str) {
    crate::test_baseline::oracle(ORACLE);
    let scratch = crate::test_scratch::Scratch::new("metadata-env");
    let root = fs::canonicalize(scratch.path()).unwrap();
    let cwd = root.join("cwd");
    let user = root.join("user");
    fs::create_dir(&cwd).unwrap();
    fs::create_dir(&user).unwrap();
    let kind = name.rsplit_once('-').unwrap().0;
    let mut env = vec![
        ("PDRS_METADATA_CASE".into(), name.into()),
        ("PDRS_METADATA_ROOT".into(), root.to_str().unwrap().into()),
    ];
    if kind != "missing-home" {
        env.push(("HOME".into(), user.to_str().unwrap().into()));
    }
    let home = match kind {
        "relative" => Some("relative/../home".into()),
        "tilde" | "missing-home" => Some("~".into()),
        "tilde-child" => Some("~/nested/../home".into()),
        "absolute" => Some(format!("{}/home/../home", root.display())),
        "empty" => Some(String::new()),
        "unset" => None,
        other => panic!("unknown case: {other}"),
    };
    if let Some(home) = home {
        env.push(("PILOT_HOME".into(), home));
    }
    crate::test_process::run_cases(WORKER, vec![(name.into(), env)]);
}

#[test]
#[ignore = "worker only; parent supplies isolated env and checks one receipt"]
fn environment_worker() {
    let name = std::env::var("PDRS_METADATA_CASE").expect("parent case required");
    let root = std::env::var("PDRS_METADATA_ROOT").expect("parent scratch required");
    let cwd = Path::new(&root).join("cwd");
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    let mut expected = if name.starts_with("unset-") || name == "missing-home-normal" {
        // Existing Rust safety gates: no implicit default home, and no tilde
        // expansion without HOME. TS's default home remains in the oracle.
        Value::Null
    } else {
        oracle["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap()["boot"]["outcome"]
            .clone()
    };
    if expected.get("error").is_some() {
        // io::Error has a kind and Display, not JS name/code/syscall properties.
        assert_eq!(expected["error"]["code"], "ENOENT");
        expected = json!({"error": expected["error"]["message"]});
    }
    let original_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&cwd).unwrap();
    if name.ends_with("-deleted") {
        fs::remove_dir(&cwd).unwrap();
        assert!(std::env::current_dir().is_err(), "deleted cwd setup failed");
    }
    let result = Metadata::from_env();
    let outcome = match result {
        Ok(Some(metadata)) => json!({
            "home": metadata.home.to_string_lossy().replace(&root, "<root>"),
            "cwd": metadata.cwd.to_string_lossy().replace(&root, "<root>"),
        }),
        Ok(None) => Value::Null,
        Err(error) => {
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
            json!({"error": error.to_string()})
        }
    };
    assert_eq!(outcome, expected, "metadata from_env matrix: {name}");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (downstream, down_peer) = tokio::io::duplex(64);
    let (upstream, up_peer) = tokio::io::duplex(64);
    drop((down_peer, up_peer));
    let paired = runtime.block_on(crate::pairing::run(downstream, upstream));
    if let Some(error) = expected.get("error") {
        assert!(paired.is_err(), "pairing propagates cwd failure: {name}");
        let actual = paired.unwrap_err();
        assert_eq!(actual.kind(), io::ErrorKind::NotFound);
        assert_eq!(
            actual.to_string(),
            error.as_str().unwrap(),
            "pairing preserves cwd error message: {name}"
        );
    } else {
        assert!(
            paired.is_ok(),
            "pairing normal/disabled initialization: {name}"
        );
    }
    std::env::set_current_dir(original_cwd).unwrap();
    println!("METADATA_ENV_MATCH {name}: {outcome}");
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
case!(tilde_normal_cwd, "tilde-normal");
case!(tilde_deleted_cwd, "tilde-deleted");
case!(tilde_child_normal_cwd, "tilde-child-normal");
case!(tilde_child_deleted_cwd, "tilde-child-deleted");
case!(absolute_normal_cwd, "absolute-normal");
case!(absolute_deleted_cwd, "absolute-deleted");
case!(unset_normal_cwd, "unset-normal");
case!(unset_deleted_cwd, "unset-deleted");
case!(empty_normal_cwd, "empty-normal");
case!(empty_deleted_cwd, "empty-deleted");
case!(missing_home_normal_cwd, "missing-home-normal");

#[test]
#[ignore = "requires Node v22.22.0 and PDRS_BASELINE"]
fn metadata_from_env_regenerates_identically() {
    crate::test_baseline::regenerates_identically("metadata-from-env");
}
