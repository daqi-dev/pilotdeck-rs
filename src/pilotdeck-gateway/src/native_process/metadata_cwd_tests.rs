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
        options: options.clone(),
        env: options.environment(Env::new()),
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
