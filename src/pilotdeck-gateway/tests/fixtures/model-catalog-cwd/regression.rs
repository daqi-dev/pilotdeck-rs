//! Cwd/environment changes run only in bounded, isolated child processes.
use super::native_metadata;
use crate::{config::Env, dispatch::Route, metadata::NativeFiles};
use pilotdeck_protocol::parse_json;
use serde_json::{Value, json};
use std::{fs, path::Path};

const ORACLE: &str = include_str!("oracle.json");
const WORKER: &str = "model_catalog::cwd_tests::environment_worker";

fn matrix(name: &str) {
    crate::test_baseline::oracle(ORACLE);
    let scratch = crate::test_scratch::Scratch::new("model-cwd");
    let root = fs::canonicalize(scratch.path()).expect("#946 cwd fixture setup/value required");
    let cwd = root.join("cwd");
    let user = root.join("user");
    fs::create_dir(&cwd).expect("#946 cwd fixture setup/value required");
    fs::create_dir(&user).expect("#946 cwd fixture setup/value required");
    let kind = name
        .rsplit_once('-')
        .expect("#946 cwd fixture setup/value required")
        .0;
    let mut env = vec![
        ("PDRS_MODEL_CWD_CASE".into(), name.into()),
        (
            "PDRS_MODEL_CWD_ROOT".into(),
            root.to_str()
                .expect("#946 cwd fixture setup/value required")
                .into(),
        ),
    ];
    if kind != "missing-home" {
        env.push((
            "HOME".into(),
            user.to_str()
                .expect("#946 cwd fixture setup/value required")
                .into(),
        ));
    }
    let (input, home) = match kind {
        "relative" => (Some("../home".into()), root.join("home")),
        "empty" => (Some(String::new()), cwd),
        "absolute" => (
            Some(format!("{}/home/../home", root.display())),
            root.join("home"),
        ),
        "tilde" => (Some("~".into()), user),
        "tilde-child" => (Some("~/nested/../home".into()), user.join("home")),
        "unset" | "missing-home" => (None, user.join(".pilotdeck")),
        other => panic!("unknown case: {other}"),
    };
    if let Some(input) = input {
        env.push(("PILOT_HOME".into(), input));
    }
    fs::create_dir_all(&home).expect("#946 cwd fixture setup/value required");
    fs::write(home.join("pilotdeck.yaml"), include_str!("config.json"))
        .expect("#946 cwd fixture setup/value required");
    env.push((
        "PDRS_MODEL_EXPECT_HOME".into(),
        home.to_str()
            .expect("#946 cwd fixture setup/value required")
            .into(),
    ));
    crate::test_process::run_cases(WORKER, vec![(name.into(), env)]);
}

fn frame(route: Route) -> Value {
    match route {
        Route::Forward => json!("forward"),
        Route::LocalResponse(bytes) => {
            serde_json::from_slice(&bytes).expect("#946 cwd fixture setup/value required")
        }
    }
}

#[test]
#[ignore = "worker only; parent supplies isolated env and checks one receipt"]
fn environment_worker() {
    let name = std::env::var("PDRS_MODEL_CWD_CASE").expect("parent case required");
    let root = std::env::var("PDRS_MODEL_CWD_ROOT").expect("parent scratch required");
    let home =
        std::env::var("PDRS_MODEL_EXPECT_HOME").expect("#946 cwd fixture setup/value required");
    let cwd = Path::new(&root).join("cwd");
    let oracle: Value =
        serde_json::from_str(ORACLE).expect("#946 cwd fixture setup/value required");
    let deleted = name.ends_with("-deleted");
    let missing_home = name.starts_with("missing-home-");
    let needs_cwd = name.starts_with("relative-") || name.starts_with("empty-");
    let env: Env = std::env::vars().collect();
    let original_cwd = std::env::current_dir().expect("#946 cwd fixture setup/value required");
    std::env::set_current_dir(&cwd).expect("#946 cwd fixture setup/value required");
    if deleted {
        fs::remove_dir_all(&cwd).expect("#946 cwd fixture setup/value required");
        assert!(std::env::current_dir().is_err(), "deleted cwd setup failed");
    }
    let actual = match native_metadata(&env) {
        Ok(Some(metadata)) => json!({"home": metadata.home, "cwd": metadata.cwd}),
        Ok(None) => Value::Null,
        Err(error) => json!({
            "name": error.name, "code": error.code,
            "message": error.message.to_utf8().expect("#946 cwd fixture setup/value required"),
        }),
    };
    let row = oracle["cases"]
        .as_array()
        .expect("#946 cwd fixture setup/value required")
        .iter()
        .find(|row| row["name"] == name);
    let expected = if deleted && needs_cwd {
        let error = &row.expect("#946 cwd fixture setup/value required")["outcome"]["error"];
        json!({"name": error["name"], "code": error["code"], "message": error["message"]})
    } else if missing_home || deleted {
        // Absolute homes succeed in TS, but native Metadata still needs cwd.
        // Preserve the existing host fallback: A4-MODEL-CATALOG-CWD-001.
        Value::Null
    } else {
        json!({"home": home, "cwd": cwd})
    };
    crate::negative_assert_eq!(actual, expected, "native metadata matrix: {name}");

    let request = parse_json(
        &json!({
            "type": "request", "id": name, "method": "model_catalog_list", "params": {},
        })
        .to_string(),
    )
    .expect("#946 cwd fixture setup/value required");
    let expected_frame = if missing_home || (deleted && !needs_cwd) {
        json!("forward")
    } else {
        row.expect("#946 cwd fixture setup/value required")["frame"].clone()
    };
    crate::negative_assert_eq!(
        frame(crate::dispatch::route_catalog::<NativeFiles>(
            &request, None, &env
        )),
        expected_frame,
        "catalog route matrix: {name}"
    );
    if deleted || missing_home {
        let request = parse_json(
            &json!({
                "type": "request", "id": name, "method": "session_model_get",
                "params": {"projectKey": root, "sessionKey": "isolated"},
            })
            .to_string(),
        )
        .expect("#946 cwd fixture setup/value required");
        crate::negative_assert_eq!(
            frame(crate::dispatch::route_with_metadata::<NativeFiles>(
                &request, None
            )),
            expected_frame,
            "session metadata route matrix: {name}"
        );
    }
    std::env::set_current_dir(original_cwd).expect("#946 cwd fixture setup/value required");
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
case!(empty_normal_cwd, "empty-normal");
case!(empty_deleted_cwd, "empty-deleted");
case!(absolute_normal_cwd, "absolute-normal");
case!(absolute_deleted_cwd, "absolute-deleted");
case!(tilde_normal_cwd, "tilde-normal");
case!(tilde_deleted_cwd, "tilde-deleted");
case!(tilde_child_normal_cwd, "tilde-child-normal");
case!(tilde_child_deleted_cwd, "tilde-child-deleted");
case!(unset_normal_cwd, "unset-normal");
case!(unset_deleted_cwd, "unset-deleted");
case!(missing_home_normal_cwd, "missing-home-normal");
case!(missing_home_deleted_cwd, "missing-home-deleted");

#[test]
#[ignore = "requires Node v22.22.0 and PDRS_BASELINE"]
fn model_catalog_cwd_regenerates_identically() {
    crate::test_baseline::regenerates_identically("model-catalog-cwd");
}
