//! All cwd changes stay in bounded child processes; each assertion identifies #879.
#[test]
#[ignore = "requires Node v22.22.0 and PDRS_BASELINE"]
fn metadata_process_cwd_regenerates_identically() {
    crate::test_baseline::regenerates_identically("metadata-process-cwd");
}
use super::resolve_process_path;
use crate::runtime::mcp::{bridge, protocol, registry, sdk::SystemEnvironment};
use pilotdeck_protocol::{JsObject, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{fs, io, path::Path, sync::Arc};

const ORACLE: &str = include_str!("oracle.json");
const WORKER: &str = "metadata::process_cwd_tests::cwd_worker";

fn matrix(name: &str) {
    std::panic::catch_unwind(|| crate::test_baseline::oracle(ORACLE))
        .expect("#879 setup: oracle provenance");
    let scratch =
        std::panic::catch_unwind(|| crate::test_scratch::Scratch::new("metadata-process"))
            .expect("#879 setup: scratch creation");
    let root = fs::canonicalize(scratch.path()).expect("#879 setup: canonical root");
    for dir in ["cwd", "project", "home"] {
        fs::create_dir(root.join(dir)).expect("#879 setup: isolated directory");
    }
    fs::write(root.join("project/image.png"), b"fixture-image").expect("#879 setup: image fixture");
    fs::write(
        root.join("home/pilotdeck.yaml"),
        include_str!("../model-catalog-cwd/config.json"),
    )
    .expect("#879 setup: config fixture");
    std::panic::catch_unwind(|| {
        crate::test_process::run_cases(
            WORKER,
            vec![(
                name.into(),
                vec![
                    ("PDRS_METADATA_PROCESS_CASE".into(), name.into()),
                    (
                        "PDRS_METADATA_PROCESS_ROOT".into(),
                        root.to_str().expect("#879 setup: UTF-8 root").into(),
                    ),
                ],
            )],
        );
    })
    .expect("#879 child matrix failed");
}

fn path_outcome(result: io::Result<std::path::PathBuf>, root: &str) -> Value {
    match result {
        Ok(path) => json!({"path": path.to_string_lossy().replace(root, "<root>")}),
        Err(error) => {
            assert_eq!(error.kind(), io::ErrorKind::NotFound, "#879 cwd error kind");
            let (code, message) = super::process_cwd_diagnostic(&error);
            assert_eq!(
                error.to_string(),
                message,
                "#879 cwd Display matches from_env shape"
            );
            json!({"error": {"name":"Error", "code":code, "message":message, "syscall":"uv_cwd"}})
        }
    }
}

fn spec(id: &str) -> protocol::McpServerSpec {
    protocol::McpServerSpec {
        id: id.into(),
        call_timeout_ms: None,
        transport: protocol::McpTransportSpec::Stdio {
            command: "__PILOTDECK_PROJECT_ROOT__".into(),
            args: Some(vec!["__PILOTDECK_FUNASR_RUNTIME_ROOT__".into()]),
            env: None,
            cwd: None,
            per_session: false,
        },
    }
}

#[test]
#[ignore = "worker only: isolated env and bounded parent receipt"]
fn cwd_worker() {
    let name = std::env::var("PDRS_METADATA_PROCESS_CASE").expect("#879 setup: case");
    let root = std::env::var("PDRS_METADATA_PROCESS_ROOT").expect("#879 setup: root");
    let oracle: Value = serde_json::from_str(ORACLE).expect("#879 setup: oracle JSON");
    let row = oracle["cases"]
        .as_array()
        .expect("#879 setup: cases")
        .iter()
        .find(|row| row["name"] == name)
        .expect("#879 setup: oracle row");
    let kind = name.rsplit_once('-').expect("#879 setup: case suffix").0;
    let base = if kind == "absolute-base" {
        format!("{root}/project/../project")
    } else {
        "unused/../".into()
    };
    let path = match kind {
        "absolute-path" => format!("{root}/project/../project"),
        "empty" => String::new(),
        _ => "../project".into(),
    };
    let input = if kind == "absolute-base" {
        &base
    } else {
        &path
    };
    let home = Path::new(&root).join("home");
    let env =
        crate::config::Env::from([("PILOT_HOME".into(), home.to_string_lossy().into_owned())]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("#879 setup: runtime");
    let saved = std::env::current_dir().expect("#879 setup: saved cwd");
    std::env::set_current_dir(Path::new(&root).join("cwd")).expect("#879 setup: enter cwd");
    let mut store_options = crate::config::store::StoreOptions::new(&home, input, env.clone());
    // input models the cwd/project root; "." selects that real project even
    // for the empty-input case, without changing the absent-project contract.
    store_options.project = Some(".".into());
    let store = crate::config::store::ConfigStore::new(store_options)
        .expect("#879 setup: healthy store before deletion");
    if name.ends_with("-deleted") {
        fs::remove_dir(Path::new(&root).join("cwd")).expect("#879 setup: delete cwd");
        assert!(
            std::env::current_dir().is_err(),
            "#879 setup: cwd must be deleted"
        );
    }
    assert_eq!(
        path_outcome(
            resolve_process_path(Path::new(&base), Path::new(&path)),
            &root
        ),
        row["resolve"]["outcome"],
        "#879 resolver outcome: {name}"
    );
    for (entry, result) in [
        (
            "global",
            protocol::global_mcp_config_file_path(Path::new(input)),
        ),
        (
            "project",
            protocol::project_mcp_config_file_path(Path::new(input)),
        ),
    ] {
        assert_eq!(
            path_outcome(result, &root),
            row[entry]["outcome"],
            "#879 {entry} outcome: {name}"
        );
    }
    let pilot_project = match crate::config::project_config_path(Path::new(input), Some(".")) {
        Ok(path) => path_outcome(
            Ok(path.expect("#879 pilot-project must select a project")),
            &root,
        ),
        Err(error) => {
            assert!(
                !error.requires_host && error.diagnostics.is_empty(),
                "#879 pilot-project cwd is not a validation/host error"
            );
            json!({"error": {"name":error.name, "code":error.code,
                "message":error.message.to_utf8().expect("#879 pilot-project error UTF-8"), "syscall":"uv_cwd"}})
        }
    };
    assert_eq!(
        pilot_project, row["pilot-project"]["outcome"],
        "#879 pilot-project outcome: {name}"
    );
    let expected_error = row["pilot-project"]["outcome"].get("error");
    for (entry, result) in [
        (
            "load-project",
            crate::config::load_project(&home, Path::new(input), &env, Some(".")).map(|_| ()),
        ),
        (
            "load-snapshot",
            crate::config::load_snapshot(&home, Path::new(input), &env, Some("."), 1.0, "fixture")
                .map(|_| ()),
        ),
        (
            "watch-paths",
            runtime.block_on(async { store.start_watching(Default::default()).map(drop) }),
        ),
    ] {
        if let Some(expected) = expected_error {
            let error = result.expect_err(&format!(
                "#879 config must propagate cwd failure: {entry}/{name}"
            ));
            assert_eq!(
                json!({"name":error.name, "code":error.code, "message":error.message.to_utf8().expect("#879 config error UTF-8")}),
                json!({"name":expected["name"], "code":expected["code"], "message":expected["message"]}),
                "#879 config error shape: {entry}/{name}"
            );
            assert!(
                !error.requires_host && error.diagnostics.is_empty(),
                "#879 config cwd is not a validation/host error"
            );
        } else {
            result.expect("#879 healthy config paths");
        }
    }
    for (entry, config_home) in [("load", Path::new(input)), ("load-project", home.as_path())] {
        let loaded =
            protocol::load_mcp_server_config(Path::new(input), config_home, &SystemEnvironment);
        if let Some(expected) = row[entry]["outcome"].get("error") {
            let error = loaded.expect_err(&format!(
                "#879 loader must propagate cwd failure: {entry}/{name}"
            ));
            assert!(
                matches!(error, protocol::McpConfigLoadError::Cwd(_)),
                "#879 loader cwd variant: {entry}/{name}"
            );
            assert_eq!(
                error.to_string(),
                expected["message"],
                "#879 loader error shape: {entry}/{name}"
            );
        } else {
            let loaded = loaded.expect("#879 healthy MCP loader");
            assert!(
                loaded.servers.is_empty() && loaded.diagnostics.is_empty(),
                "#879 missing MCP config remains empty"
            );
        }
    }
    for id in ["funasr", "funasr-home", "browser-use", "ordinary"] {
        let absolute_project = format!("{root}/project");
        let (spec_id, project_root) = if id == "funasr-home" {
            ("funasr", &absolute_project)
        } else {
            (id, input)
        };
        let result = registry::patch_project_scoped_mcp_spec(
            spec(spec_id),
            project_root,
            input,
            &Default::default(),
        );
        let actual = match result {
            Err(error) => path_outcome(Err(error), &root),
            Ok(patched) => {
                let protocol::McpTransportSpec::Stdio {
                    cwd, command, args, ..
                } = patched.transport
                else {
                    panic!("#879 patched transport stays stdio");
                };
                json!({"cwd":cwd.map(|s|s.replace(&root,"<root>")), "command":command.replace(&root,"<root>"),
                    "args":args.map(|items|items.into_iter().map(|s|s.replace(&root,"<root>")).collect::<Vec<_>>())})
            }
        };
        let expected = if id == "ordinary" {
            json!({"cwd":null,"command":"__PILOTDECK_PROJECT_ROOT__","args":["__PILOTDECK_FUNASR_RUNTIME_ROOT__"]})
        } else {
            row[if id == "browser-use" { "browser" } else { id }]["outcome"].clone()
        };
        assert_eq!(actual, expected, "#879 patch outcome: {id}/{name}");
    }
    let remote = protocol::McpServerSpec {
        id: "funasr".into(),
        call_timeout_ms: None,
        transport: protocol::McpTransportSpec::Sse {
            url: "http://fixture.invalid/".into(),
            headers: None,
        },
    };
    assert_eq!(
        registry::patch_project_scoped_mcp_spec(remote.clone(), input, input, &Default::default())
            .expect("#879 remote spec must not need cwd"),
        remote,
        "#879 remote spec remains unchanged"
    );
    let provider =
        registry::ProjectMcpRuntimeProvider::new(registry::ProjectMcpRuntimeProviderOptions {
            project_root: input.clone(),
            pilot_home: home.to_string_lossy().into_owned(),
            create_runtime: None,
            proxy: None,
            on_diagnostic: None,
            environment: Arc::new(SystemEnvironment),
            builtin_paths: Default::default(),
        });
    let result = provider.per_session_server_specs(&JsObject::new(), false);
    if let Some(expected) = expected_error {
        let error = result.expect_err("#879 provider must propagate cwd failure");
        let js = error.js_error().expect("#879 provider retains JS error");
        assert_eq!(
            json!({"name":js.name,"code":js.code,"message":js.message.to_utf8().expect("#879 provider message UTF-8")}),
            json!({"name":expected["name"],"code":expected["code"],"message":expected["message"]}),
            "#879 provider error shape: {name}"
        );
    } else {
        assert!(
            result.expect("#879 healthy provider paths").is_none(),
            "#879 empty provider remains empty"
        );
    }
    let raw = parse_json(r#"[{"type":"text","text":"[shot](./image.png)"}]"#)
        .expect("#879 setup: image blocks");
    let content =
        pilotdeck_protocol::JsValue::Array(bridge::marshal_mcp_content(&raw, Some(input)));
    let actual: Value = serde_json::from_str(
        &stringify_json(&content)
            .expect("#879 bridge stringify")
            .expect("#879 bridge JSON value"),
    )
    .expect("#879 bridge JSON");
    assert_eq!(
        json!({"content":actual}),
        row["bridge"]["outcome"],
        "#879 bridge outcome: {name}"
    );
    std::env::set_current_dir(saved).expect("#879 restore cwd");
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
case!(relative_normal, "relative-normal");
case!(relative_deleted, "relative-deleted");
case!(empty_normal, "empty-normal");
case!(empty_deleted, "empty-deleted");
case!(absolute_base_normal, "absolute-base-normal");
case!(absolute_base_deleted, "absolute-base-deleted");
case!(absolute_path_normal, "absolute-path-normal");
case!(absolute_path_deleted, "absolute-path-deleted");
