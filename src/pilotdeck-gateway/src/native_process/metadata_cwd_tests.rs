use super::*;
use crate::runtime::{
    project::{ProjectRuntimeFactory, ProjectRuntimeRegistry, ResourceBundle, RuntimeResult},
    session::{SessionContext, SessionFactory, SessionOwner, SessionOwnerOptions},
    turn::{gateway::GatewayRequests, submit_host::SubmitHostServices},
};

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/native-metadata-cwd/oracle.json"
    ))
    .expect("#1154 valid TS oracle JSON")
}

#[test]
fn matrix_matches_ts() {
    crate::test_baseline::oracle_bounded(include_str!(
        "../../tests/fixtures/native-metadata-cwd/oracle.json"
    ));
    let oracle = fixture();
    negative_assert_eq!(oracle["node"], "v22.22.0", "#1154 pinned TS Node");
    negative_assert_eq!(
        oracle["cases"].as_array().expect("#1154 TS cases").len(),
        8,
        "#1154 matrix cardinality"
    );
    negative_assert_eq!(oracle["sessionCwd"], ".", "#1154 TS stores raw session cwd");
    for name in [
        "relative_project",
        "relative_other",
        "absolute_other",
        "absolute_deleted",
        "relative_deleted",
        "symlink_scan",
        "collision_deleted",
        "collision_other",
    ] {
        crate::test_process::run_cases(
            &format!("native_process::tests::metadata_cwd::{name}"),
            vec![(name.into(), vec![])],
        );
    }
}

#[test]
#[ignore = "requires pinned Node and explicit PDRS_BASELINE"]
fn regenerates_identically() {
    crate::test_baseline::regenerates_identically("native-metadata-cwd");
}

struct NoResources;
impl ProjectRuntimeFactory for NoResources {
    fn stage(&self, _: &Path, _: &mut ResourceBundle) -> RuntimeResult {
        panic!("#1154 metadata must not stage project resources")
    }
}
impl SessionFactory for NoResources {
    fn create(
        &self,
        _: SessionContext,
        _: Option<crate::runtime::session::LiveSessionHandle>,
    ) -> crate::runtime::refresh::BoxFuture<
        Result<crate::runtime::session::LiveSessionHandle, crate::runtime::project::RuntimeError>,
    > {
        panic!("#1154 metadata must not create sessions")
    }
}
struct Work(PathBuf);
impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.0);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn observe(name: &str) -> (Value, Value) {
    let oracle = fixture();
    let row = oracle["cases"]
        .as_array()
        .expect("#1154 oracle cases")
        .iter()
        .find(|r| r["name"] == name.replace('_', "-"))
        .expect("#1154 named TS case");
    let path = std::env::temp_dir().join(format!("pdrs-1154-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).expect("#1154 create isolated root");
    let work = Work(path.canonicalize().expect("#1154 canonical isolated root"));
    let case = work.0.join(name.replace('_', "-"));
    for dir in ["home", "project", "other", "deleted"] {
        std::fs::create_dir_all(case.join(dir)).expect("#1154 create isolated directory");
    }
    for file in ["token", "sidecar"] {
        std::fs::write(case.join(file), "fixture").expect("#1154 create isolated file");
    }
    let options = Options::parse([
        OsString::from("--home"),
        case.join("home").into_os_string(),
        OsString::from("--project"),
        case.join("project").into_os_string(),
        OsString::from("--listen"),
        OsString::from("127.0.0.1:0"),
        OsString::from("--token-file"),
        case.join("token").into_os_string(),
        OsString::from("--sidecar"),
        case.join("sidecar").into_os_string(),
    ])
    .expect("#1154 parse isolated options");
    std::fs::write(options.home.join("pilotdeck.yaml"), CONFIG)
        .expect("#1154 write isolated config");
    let storage = if name == "symlink_scan" {
        options.home.join("link-storage")
    } else {
        options
            .home
            .join("projects")
            .join(if name.starts_with("collision_") {
                "zzz-collision".into()
            } else {
                crate::metadata::project_id(&options.project)
            })
    };
    std::fs::create_dir_all(&storage).expect("#1154 create marker directory");
    if name == "symlink_scan" {
        std::fs::create_dir_all(options.home.join("projects")).expect("#1154 symlink parent");
        std::os::unix::fs::symlink(&storage, options.home.join("projects/legacy"))
            .expect("#1154 symlink registration");
    }
    let marker = row["marker"]
        .as_str()
        .expect("#1154 TS marker")
        .replace("{project}", &options.project.to_string_lossy());
    std::fs::write(storage.join(".cwd"), marker).expect("#1154 publish marker");
    if name == "absolute_deleted" || name == "collision_deleted" {
        let unrelated = options.home.join("projects/000-unrelated");
        std::fs::create_dir_all(&unrelated).expect("#1154 unrelated marker directory");
        std::fs::write(unrelated.join(".cwd"), ".").expect("#1154 unrelated relative marker");
    }
    std::fs::write(options.project.join("visible.txt"), "fixture")
        .expect("#1154 visible project file");
    if name == "collision_other" {
        let unrelated = options.home.join("projects/000-unrelated");
        std::fs::create_dir_all(&unrelated).expect("#1154 second registration directory");
        std::fs::write(
            unrelated.join(".cwd"),
            case.join("other").to_string_lossy().as_bytes(),
        )
        .expect("#1154 second absolute registration");
    }
    let mut owner_options = SessionOwnerOptions::new(Arc::new(NoResources));
    owner_options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(owner_options);
    let requests = requests::Requests {
        project: Arc::new(ProjectRuntimeRegistry::new(
            options.project.clone(),
            Arc::new(NoResources),
        )),
        options: options.clone(),
        env: options.environment(Env::new()),
        snapshot_enabled: false,
        coordinator: Arc::default(),
        runs: Arc::new(
            crate::runtime::turn::publish::FileRegistry::new(
                crate::storage::FileRunRegistry::open(options.home.join("runs.json"))
                    .expect("#1154 run file"),
            )
            .expect("#1154 run registry"),
        ),
        owner: owner.clone(),
    };
    let services = services::Services {
        options: options.clone(),
        env: options.environment(Env::new()),
        project: Arc::new(ProjectRuntimeRegistry::new(
            options.project.clone(),
            Arc::new(NoResources),
        )),
        recorders: Arc::default(),
        owner,
    };
    let cwd = row["cwd"].as_str().expect("#1154 TS cwd");
    std::env::set_current_dir(case.join(cwd)).expect("#1154 enter process cwd");
    if cwd == "deleted" {
        std::fs::remove_dir(case.join(cwd)).expect("#1154 delete process cwd");
    }
    assert_eq!(
        std::env::current_dir().is_ok(),
        row["cwdReadable"]
            .as_bool()
            .expect("#1154 TS cwd readability"),
        "#1154 actual cwd state"
    );
    let params = pilotdeck_protocol::parse_json(
        &json!({"projectKey": options.project, "sessionKey":"s", "includeDirs":false}).to_string(),
    )
    .expect("#1154 model parameters");
    let read = requests
        .request("session_model_get".into(), params.clone())
        .await;
    let submit = services.resolve_model_selection(&params).await;
    let description = requests
        .request("describe_project".into(), params.clone())
        .await;
    let file_list = requests.request("project_files_list".into(), params).await;
    let projects = requests
        .request("list_projects".into(), JsValue::Null)
        .await;
    let describe_has_storage = description
        .as_ref()
        .ok()
        .and_then(|v| v.get("createdAt"))
        .is_some_and(|v| !v.is_undefined());
    let metadata = crate::metadata::Metadata {
        home: options.home.clone(),
        cwd: PathBuf::new(),
        files: crate::metadata::NativeFiles,
    };
    let chat_uses_direct_id = metadata
        .process_chat_dir(&options.project.to_string_lossy())
        .expect("#1154 absolute project chat path")
        == options
            .home
            .join("projects")
            .join(crate::metadata::project_id(&options.project))
            .join("chats");
    std::env::set_current_dir(&work.0).expect("#1154 restore known cwd");
    let project_cwd = services.cwd().expect("#1154 native cwd service");
    negative_assert!(
        project_cwd.set(&"s".into(), &".".into()).is_ok(),
        "#1154 isolated same-project cwd accepted"
    );
    negative_assert!(
        project_cwd
            .set(
                &"s".into(),
                &case.join("other").to_string_lossy().as_ref().into()
            )
            .is_err(),
        "#1154 isolated project switch remains refused"
    );
    services
        .project
        .dispose()
        .await
        .expect("#1154 dispose unused project");
    let normalize = |result: Result<JsValue, (String, JsString)>, read: bool| match result {
        Ok(value) => {
            let source = if read {
                value.get("effective").and_then(|v| v.get("source"))
            } else {
                value.get("source")
            };
            json!({"ok":true,"source":source.and_then(JsValue::as_string).expect("#1154 successful model source").to_utf8().expect("#1154 UTF8 model source")})
        }
        Err((code, message)) => {
            json!({"ok":false,"code":code,"message":message.to_utf8().expect("#1154 UTF8 model error").replace(&work.0.to_string_lossy().to_string(), "{root}")})
        }
    };
    let count = |result: Result<JsValue, crate::runtime::connection::RequestError>, field: &str| {
        match result {
            Ok(value) => {
                let Some(JsValue::Array(items)) = value.get(field) else {
                    panic!("#1154 expected array in list result");
                };
                let mut result = json!({"ok":true,"count":items.len()});
                if field == "items" {
                    result["paths"] = json!(
                        items
                            .iter()
                            .map(|item| {
                                item.get("relativePath")
                                    .and_then(JsValue::as_string)
                                    .expect("#1154 file item has relativePath")
                                    .to_utf8()
                                    .expect("#1154 file relativePath is UTF8")
                            })
                            .collect::<Vec<_>>()
                    );
                }
                result
            }
            Err(error) => json!({"ok":false,"code":error.code,
            "message":error.message.to_utf8().expect("#1154 list UTF8 error")
                .replace(&work.0.to_string_lossy().to_string(), "{root}")}),
        }
    };
    let actual = json!({
        "describeHasStorage": describe_has_storage,
        "chatUsesDirectId": chat_uses_direct_id,
        "projectFilesList": count(file_list, "items"),
        "listProjects": count(projects, "projects"),
        "sessionModelGet": normalize(read.map_err(|e| (e.code, e.message)), true),
        "resolveTurnModelSelection": normalize(submit.map_err(|e| (e.code, e.message.into())), false),
    });
    (actual, row.clone())
}

macro_rules! case {
    ($name:ident) => {
        #[tokio::test]
        #[ignore = "isolated cwd worker launched by matrix_matches_ts"]
        async fn $name() {
            let (actual, expected) = observe(stringify!($name)).await;
            negative_assert_eq!(
                actual["sessionModelGet"],
                expected["sessionModelGet"],
                "#1154 request metadata uses process cwd"
            );
            negative_assert_eq!(
                actual["resolveTurnModelSelection"],
                expected["resolveTurnModelSelection"],
                "#1154 submit metadata uses process cwd"
            );
            negative_assert_eq!(
                actual["describeHasStorage"],
                expected["describeHasStorage"],
                "#1154 description preserves legacy candidate on cwd error"
            );
            negative_assert_eq!(
                actual["projectFilesList"],
                expected["projectFilesList"],
                "#1154 file RPC enumerates all projects"
            );
            negative_assert_eq!(
                actual["listProjects"],
                expected["listProjects"],
                "#1154 list RPC propagates cwd error"
            );
            negative_assert_eq!(
                actual["chatUsesDirectId"],
                expected["chatUsesDirectId"],
                "#1154 chat lookup stops on cwd error"
            );
            crate::test_process::receipt(stringify!($name));
        }
    };
}
case!(relative_project);
case!(relative_other);
case!(absolute_other);
case!(absolute_deleted);
case!(relative_deleted);
case!(symlink_scan);
case!(collision_deleted);
case!(collision_other);

fn archive_fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/native-archive-cwd/oracle.json"
    ))
    .expect("#1154 archive valid oracle JSON")
}

// #1154 fix-r3 (review P1-3/P3-9): the six admission cells below stay
// machine-bound to the correction entry's explicit admission lines — exact
// `marker: value` ledger lines, not a substring any other sentence could
// satisfy — so an approved or revoked registration must fail the matrix
// instead of silently drifting from the hardcoded expectation.
const ADMISSION_LEDGER: &str =
    include_str!("../../../../specs/gateway-parity/diffs/A7.2-NATIVE-ARCHIVE-CWD-001-R1.md");
const ADMISSION_CODE: &str = "gateway_request_failed";
const ADMISSION_MESSAGE: &str = "The isolated native process serves only its configured project.";

fn admission_ledger_line(marker: &str) -> Option<&'static str> {
    ADMISSION_LEDGER
        .lines()
        .find_map(|line| line.strip_prefix(marker))
}

#[test]
fn archive_matrix_matches_ts() {
    crate::test_baseline::oracle_bounded(include_str!(
        "../../tests/fixtures/native-archive-cwd/oracle.json"
    ));
    let oracle = archive_fixture();
    negative_assert_eq!(oracle["node"], "v22.22.0", "#1154 archive pinned TS Node");
    negative_assert_eq!(
        oracle["cases"]
            .as_array()
            .expect("#1154 archive TS cases")
            .len(),
        36,
        "#1154 archive matrix cardinality"
    );
    for method in ["manifest", "entries", "artifact"] {
        for key in ["missing", "null", "empty", "absolute"] {
            for cwd in ["project", "other", "deleted"] {
                let name = format!("archive_{method}_{key}_{cwd}");
                crate::test_process::run_cases(
                    &format!("native_process::tests::metadata_cwd::{name}"),
                    vec![(name, vec![])],
                );
            }
        }
    }
}

#[test]
#[ignore = "requires pinned Node and explicit PDRS_BASELINE"]
fn archive_regenerates_identically() {
    crate::test_baseline::regenerates_identically("native-archive-cwd");
}

async fn observe_archive(name: &str) {
    let oracle = archive_fixture();
    let parts: Vec<_> = name.split('_').collect();
    assert_eq!(parts.len(), 4, "#1154 archive case dimensions");
    let (method, key, cwd) = (parts[1], parts[2], parts[3]);
    let row = oracle["cases"]
        .as_array()
        .expect("#1154 archive cases array")
        .iter()
        .find(|r| {
            r["name"]
                == name
                    .strip_prefix("archive_")
                    .expect("#1154 archive case prefix")
        })
        .expect("#1154 archive named case");
    negative_assert_eq!(row["method"], method, "#1154 archive method dimension");
    negative_assert_eq!(row["key"], key, "#1154 archive key dimension");
    negative_assert_eq!(row["cwd"], cwd, "#1154 archive cwd dimension");
    negative_assert_eq!(
        row["cwdReadable"],
        cwd != "deleted",
        "#1154 archive cwd state dimension"
    );
    let path = std::env::temp_dir().join(format!("pdrs-1154-archive-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).expect("#1154 archive isolated root");
    let work = Work(path.canonicalize().expect("#1154 archive canonical root"));
    for dir in ["home", "project", "other", "deleted"] {
        std::fs::create_dir_all(work.0.join(dir)).expect("#1154 archive directory");
    }
    for file in ["token", "sidecar"] {
        std::fs::write(work.0.join(file), "fixture").expect("#1154 archive file");
    }
    let options = Options::parse([
        OsString::from("--home"),
        work.0.join("home").into_os_string(),
        OsString::from("--project"),
        work.0.join("project").into_os_string(),
        OsString::from("--listen"),
        OsString::from("127.0.0.1:0"),
        OsString::from("--token-file"),
        work.0.join("token").into_os_string(),
        OsString::from("--sidecar"),
        work.0.join("sidecar").into_os_string(),
    ])
    .expect("#1154 archive options");
    std::fs::write(options.home.join("pilotdeck.yaml"), CONFIG).expect("#1154 archive config");
    let storage = options
        .home
        .join("projects")
        .join(crate::metadata::project_id(&options.project));
    let chat = storage.join("chats");
    let artifacts = options.project.join(".pilotdeck/tool-results/fixture");
    std::fs::create_dir_all(&chat).expect("#1154 archive chats directory");
    std::fs::create_dir_all(&artifacts).expect("#1154 archive artifacts directory");
    std::fs::write(
        storage.join(".cwd"),
        options.project.to_string_lossy().as_bytes(),
    )
    .expect("#1154 archive marker");
    std::fs::write(chat.join("fixture.jsonl"), json!({"type":"message","sessionId":"fixture","turnId":"","sequence":0,"createdAt":"2026-10-15T00:00:00Z","payload":{"text":"hello"}}).to_string() + "\n").expect("#1154 archive transcript");
    std::fs::write(artifacts.join("fixture.txt"), "fixture artifact")
        .expect("#1154 archive artifact");
    let mut owner_options = SessionOwnerOptions::new(Arc::new(NoResources));
    owner_options.idle_sweep_interval_ms = 0;
    let requests = requests::Requests {
        project: Arc::new(ProjectRuntimeRegistry::new(
            options.project.clone(),
            Arc::new(NoResources),
        )),
        options: options.clone(),
        env: options.environment(Env::new()),
        snapshot_enabled: false,
        coordinator: Arc::default(),
        runs: Arc::new(
            crate::runtime::turn::publish::FileRegistry::new(
                crate::storage::FileRunRegistry::open(options.home.join("runs.json"))
                    .expect("#1154 archive run file"),
            )
            .expect("#1154 archive run registry"),
        ),
        owner: SessionOwner::new(owner_options),
    };
    let mut params = json!({"sessionKey":"fixture"});
    match key {
        "missing" => {}
        "null" => params["projectKey"] = Value::Null,
        "empty" => params["projectKey"] = json!(""),
        "absolute" => params["projectKey"] = json!(options.project),
        _ => panic!("#1154 archive unknown key kind"),
    }
    if method == "artifact" {
        params["artifactName"] = json!("fixture.txt");
    }
    let params =
        pilotdeck_protocol::parse_json(&params.to_string()).expect("#1154 archive wire params");
    std::env::set_current_dir(work.0.join(cwd)).expect("#1154 archive enter cwd");
    if cwd == "deleted" {
        std::fs::remove_dir(work.0.join(cwd)).expect("#1154 archive delete cwd");
    }
    assert_eq!(
        std::env::current_dir().is_ok(),
        row["cwdReadable"]
            .as_bool()
            .expect("#1154 archive TS cwd readability"),
        "#1154 archive actual cwd state"
    );
    let result = requests
        .request(format!("native_archive_{method}").into(), params)
        .await;
    std::env::set_current_dir(&work.0).expect("#1154 archive restore cwd");
    let actual = match result {
        Ok(value) => {
            json!({"ok":true,"result":serde_json::from_str::<Value>(&pilotdeck_protocol::stringify_json(&value).expect("#1154 archive serializable result").expect("#1154 archive defined result")).expect("#1154 archive result JSON")})
        }
        Err(error) => {
            json!({"ok":false,"code":error.code,"message":error.message.to_utf8().expect("#1154 archive UTF8 error")})
        }
    };
    let expected = if key == "empty" && cwd != "project" {
        // Existing isolated admission policy, recorded separately from TS parity.
        // specs/gateway-parity/diffs/A7.2-NATIVE-ARCHIVE-CWD-001-R1.md
        negative_assert!(
            ADMISSION_LEDGER
                .lines()
                .any(|line| line == "original: A7.2-NATIVE-ARCHIVE-CWD-001"),
            "#1154 archive admission expectation stays bound to its ledger"
        );
        negative_assert_eq!(
            admission_ledger_line("admission-code: "),
            Some(ADMISSION_CODE),
            "#1154 archive admission expectation stays bound to its ledger"
        );
        negative_assert_eq!(
            admission_ledger_line("admission-message: "),
            Some(ADMISSION_MESSAGE),
            "#1154 archive admission expectation stays bound to its ledger"
        );
        json!({"ok":false,"code":ADMISSION_CODE,"message":ADMISSION_MESSAGE})
    } else if row["ok"] == true {
        json!({"ok":true,"result":row["result"]})
    } else {
        json!({"ok":false,"code":row["code"],"message":row["message"]})
    };
    negative_assert_eq!(
        actual,
        expected,
        "#1154 archive configured fallback and explicit key boundary"
    );
    crate::test_process::receipt(name);
}

macro_rules! archive_case {
    ($name:ident) => {
        #[tokio::test]
        #[ignore = "isolated cwd worker launched by archive_matrix_matches_ts"]
        async fn $name() {
            observe_archive(stringify!($name)).await;
        }
    };
}
archive_case!(archive_manifest_missing_project);
archive_case!(archive_manifest_missing_other);
archive_case!(archive_manifest_missing_deleted);
archive_case!(archive_manifest_null_project);
archive_case!(archive_manifest_null_other);
archive_case!(archive_manifest_null_deleted);
archive_case!(archive_manifest_empty_project);
archive_case!(archive_manifest_empty_other);
archive_case!(archive_manifest_empty_deleted);
archive_case!(archive_manifest_absolute_project);
archive_case!(archive_manifest_absolute_other);
archive_case!(archive_manifest_absolute_deleted);
archive_case!(archive_entries_missing_project);
archive_case!(archive_entries_missing_other);
archive_case!(archive_entries_missing_deleted);
archive_case!(archive_entries_null_project);
archive_case!(archive_entries_null_other);
archive_case!(archive_entries_null_deleted);
archive_case!(archive_entries_empty_project);
archive_case!(archive_entries_empty_other);
archive_case!(archive_entries_empty_deleted);
archive_case!(archive_entries_absolute_project);
archive_case!(archive_entries_absolute_other);
archive_case!(archive_entries_absolute_deleted);
archive_case!(archive_artifact_missing_project);
archive_case!(archive_artifact_missing_other);
archive_case!(archive_artifact_missing_deleted);
archive_case!(archive_artifact_null_project);
archive_case!(archive_artifact_null_other);
archive_case!(archive_artifact_null_deleted);
archive_case!(archive_artifact_empty_project);
archive_case!(archive_artifact_empty_other);
archive_case!(archive_artifact_empty_deleted);
archive_case!(archive_artifact_absolute_project);
archive_case!(archive_artifact_absolute_other);
archive_case!(archive_artifact_absolute_deleted);
