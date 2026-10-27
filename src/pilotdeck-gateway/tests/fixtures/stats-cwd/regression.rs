//! Cwd/environment changes run only in bounded, isolated child processes.
use super::{StatsConfig, TokenStatsCollector};
use crate::{
    host::ModuleError,
    runtime::model::router::{RouterConfig, RouterDeps, RouterModelInvocation, RouterRuntime},
};
use pilotdeck_protocol::{JsString, JsValue};
use serde_json::{Value, json};
use std::{fs, path::Path, sync::Arc};

const ORACLE: &str = include_str!("oracle.json");
const WORKER: &str = "runtime::model::stats::cwd_tests::environment_worker";

fn matrix(name: &str) {
    crate::test_baseline::oracle(ORACLE);
    // The TS oracle runs each entry in a fresh child with its own temp root
    // (`generate.mts --child <case> <entry>`). Mirror that split: the collector
    // entry must not create the file the runtime entry then "observes", and
    // each entry needs a clean home (#879b review P1).
    for entry in ["collector", "runtime"] {
        let scratch = crate::test_scratch::Scratch::new("stats-cwd");
        let root = fs::canonicalize(scratch.path()).expect("#879 stats cwd scratch");
        fs::create_dir(root.join("cwd")).expect("#879 stats cwd directory");
        fs::create_dir(root.join("user")).expect("#879 stats user directory");
        let env = environment(name, entry, &root);
        crate::test_process::run_cases(WORKER, vec![(name.into(), env)]);
    }
}

fn environment(name: &str, entry: &str, root: &Path) -> Vec<(String, String)> {
    let kind = name
        .rsplit_once('-')
        .expect("#879 stats cwd case has a state suffix")
        .0;
    let user = root.join("user");
    let input: Option<String> = match kind {
        "relative" => Some("../home".into()),
        "empty" => Some(String::new()),
        "absolute" => Some(format!("{}/home/../home", root.display())),
        "tilde" => Some("~".into()),
        "tilde-child" => Some("~/nested/../home".into()),
        "unset" => None,
        "empty-home-tilde" => Some("~".into()),
        "empty-home-unset" => None,
        "empty-home-child" => Some("~/data".into()),
        "file-path" => Some("../home".into()),
        "disabled" => Some("../home".into()),
        other => panic!("#879 unknown stats cwd case: {other}"),
    };
    let mut env = vec![
        ("PDRS_STATS_CWD_CASE".into(), name.into()),
        ("PDRS_STATS_CWD_ENTRY".into(), entry.into()),
        (
            "PDRS_STATS_CWD_ROOT".into(),
            root.to_str().expect("#879 stats cwd root utf8").into(),
        ),
    ];
    if let Some(input) = input {
        env.push(("PILOT_HOME".into(), input));
    }
    // TS `os.homedir()` returns "" when HOME is set to an empty string; the
    // bare `~` cases pin that separate source (TokenStatsCollector.ts:90-96).
    let home_value = if kind.starts_with("empty-home-") {
        String::new()
    } else {
        user.to_str().expect("#879 stats cwd user utf8").into()
    };
    env.push(("HOME".into(), home_value));
    env
}

struct UnusedInvoker;

impl RouterModelInvocation for UnusedInvoker {
    fn get_capabilities(
        &self,
        _provider: &JsString,
        _model: &JsString,
    ) -> Result<JsValue, ModuleError> {
        Ok(JsValue::Undefined)
    }

    fn get_multimodal(
        &self,
        _provider: &JsString,
        _model: &JsString,
    ) -> Result<JsValue, ModuleError> {
        Ok(JsValue::Undefined)
    }
}

fn outcome<T>(result: std::io::Result<T>, ok: impl FnOnce(T) -> Value) -> Value {
    match result {
        Ok(value) => ok(value),
        Err(error) => {
            let code = match error.kind() {
                std::io::ErrorKind::NotFound => "ENOENT",
                std::io::ErrorKind::PermissionDenied => "EACCES",
                _ => "UNKNOWN",
            };
            json!({"error": {
                "name": "Error",
                "code": code,
                "message": error.to_string(),
                "syscall": "uv_cwd",
            }})
        }
    }
}

fn normalize(value: Option<String>, root: &str) -> Option<String> {
    value.map(|value| value.split(root).collect::<Vec<_>>().join("<root>"))
}

#[test]
#[ignore = "worker only; parent supplies isolated env and checks one receipt"]
fn environment_worker() {
    let name = std::env::var("PDRS_STATS_CWD_CASE").expect("#879 stats cwd case required");
    let entry = std::env::var("PDRS_STATS_CWD_ENTRY").expect("#879 stats cwd entry required");
    let root = std::env::var("PDRS_STATS_CWD_ROOT").expect("#879 stats cwd root required");
    let kind = name
        .rsplit_once('-')
        .expect("#879 stats cwd worker case suffix")
        .0;
    let oracle: Value =
        serde_json::from_str(ORACLE).expect("#879 stats cwd oracle must be valid JSON");
    let row = oracle["cases"]
        .as_array()
        .expect("#879 stats cwd oracle cases array")
        .iter()
        .find(|row| row["name"] == name)
        .expect("#879 stats cwd oracle row");

    let cwd = Path::new(&root).join("cwd");
    let original_cwd = std::env::current_dir().expect("#879 stats cwd original directory");
    std::env::set_current_dir(&cwd).expect("#879 stats cwd enter directory");
    if name.ends_with("-deleted") {
        fs::remove_dir_all(&cwd).expect("#879 stats cwd remove directory");
        assert!(
            std::env::current_dir().is_err(),
            "#879 stats cwd deletion did not take effect: {name}"
        );
    }

    let enabled = kind != "disabled";
    let file_path = (kind == "file-path").then(|| Path::new(&root).join("custom/stats.jsonl"));
    let config = StatsConfig {
        enabled,
        file_path: file_path.clone(),
        ..StatsConfig::default()
    };

    if entry == "collector" {
        let actual_collector = outcome(
            TokenStatsCollector::with_clock(Some(&config), None, Arc::new(|| 0.0)),
            |collector| {
                let actual = normalize(
                    collector
                        .jsonl_path
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned()),
                    &root,
                );
                let exists = collector
                    .jsonl_path
                    .as_ref()
                    .is_some_and(|path| path.exists());
                json!({"result": {"journal": actual, "exists": exists}})
            },
        );
        crate::negative_assert_eq!(
            actual_collector,
            row["collector"]["outcome"],
            "#879 stats cwd collector matrix: {name}"
        );
    } else if entry == "runtime" {
        let runtime_config = RouterConfig {
            enabled: true,
            stats: config,
            ..RouterConfig::default()
        };
        let actual_runtime = match RouterRuntime::with_deps(
            runtime_config,
            RouterDeps::new(Arc::new(UnusedInvoker)),
        ) {
            Ok(runtime) => {
                // Read the runtime-owned collector's real selection; the
                // oracle path above is only the expected value.
                let journal = runtime.stats_jsonl_path();
                let actual = normalize(
                    journal
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned()),
                    &root,
                );
                let exists = journal.as_ref().is_some_and(|path| path.exists());
                runtime.shutdown();
                json!({"result": {"journal": actual, "exists": exists}})
            }
            Err(error) => json!({"error": {
                "name": "Error",
                "code": error.code,
                "message": error.message.to_utf8().unwrap_or_default(),
                "syscall": "uv_cwd",
            }}),
        };
        crate::negative_assert_eq!(
            actual_runtime,
            row["runtime"]["outcome"],
            "#879 stats cwd runtime matrix: {name}"
        );
    } else {
        panic!("#879 unknown stats cwd entry: {entry}");
    }

    std::env::set_current_dir(&original_cwd).expect("#879 stats cwd restore directory");
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
case!(empty_home_tilde_normal_cwd, "empty-home-tilde-normal");
case!(empty_home_tilde_deleted_cwd, "empty-home-tilde-deleted");
case!(empty_home_unset_normal_cwd, "empty-home-unset-normal");
case!(empty_home_unset_deleted_cwd, "empty-home-unset-deleted");
case!(empty_home_child_normal_cwd, "empty-home-child-normal");
case!(empty_home_child_deleted_cwd, "empty-home-child-deleted");
case!(file_path_normal_cwd, "file-path-normal");
case!(file_path_deleted_cwd, "file-path-deleted");
case!(disabled_normal_cwd, "disabled-normal");
case!(disabled_deleted_cwd, "disabled-deleted");

#[test]
#[ignore = "requires Node v22.22.0 and PDRS_BASELINE"]
fn stats_cwd_regenerates_identically() {
    crate::test_baseline::regenerates_identically("stats-cwd");
}
