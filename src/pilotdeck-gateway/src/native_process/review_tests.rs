use super::*;

#[test]
fn production_turn_ports_share_cancellation() {
    use crate::host::modules::tool_context::HostToolCheckpoint;
    use crate::runtime::{
        hooks::Cancellation,
        session::{SessionContext, factory::SessionBinding},
    };
    use pilotdeck_protocol::admission::ExecutionIdentity;
    let scratch = Scratch::new();
    let options = Options::parse(scratch.args()).expect("#1071 cancellation options");
    let recorder = crate::runtime::turn::live::test_recorder();
    let binding = SessionBinding {
        context: SessionContext {
            session_key: "session".into(),
            project_key: None,
            channel_key: "probe".into(),
            allowed_tools: None,
            disallowed_tools: None,
        },
        recorder: recorder.clone(),
        checkpoint: HostToolCheckpoint::new(None, &[]).expect("#1071 cancellation checkpoint"),
        projection: Arc::default(),
    };
    let tools = tools::SessionTools::new(
        &options,
        &Env::new(),
        binding,
        JsObject::new(),
        Arc::default(),
        Arc::default(),
    )
    .expect("#1071 production session tools");
    let signal = Cancellation::new();
    let ports = assembly::turn_ports(
        &tools,
        &ExecutionIdentity {
            run_id: "run".into(),
            operation_id: "operation".into(),
            request_id: "request".into(),
            session_id: "session".into(),
            turn_id: "turn".into(),
        },
        recorder,
        signal.clone(),
    );
    negative_assert!(
        !ports.interaction.signal.is_cancelled(),
        "#1071 production port starts uncancelled"
    );
    signal.cancel();
    negative_assert!(
        ports.interaction.signal.is_cancelled(),
        "#1071 production turn ports share cancellation"
    );
}

#[tokio::test]
async fn production_cleanup_matrix() {
    for mode in [
        "ok",
        "session-error",
        "project-error",
        "both-errors",
        "release",
        "cancel",
        "never",
    ] {
        let scratch = Scratch::new();
        let options = Options::parse(scratch.args()).expect("#1071 cleanup options");
        std::fs::write(options.home.join("pilotdeck.yaml"), CONFIG)
            .expect("#1071 cleanup configuration");
        let calls = Arc::new(Mutex::new(Vec::<&str>::new()));
        let warnings = Arc::new(Mutex::new(Vec::<String>::new()));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let observed = calls.clone();
        let entered_hook = entered.clone();
        let release_hook = release.clone();
        let reported = warnings.clone();
        let host = assembly::start_observed(
            options.clone(),
            options.environment(Env::new()),
            Arc::new(move |step, result| {
                observed
                    .lock()
                    .expect("#1071 cleanup calls lock")
                    .push(step);
                let entered = entered_hook.clone();
                let release = release_hook.clone();
                Box::pin(async move {
                    result?; // The actual production dispose must finish first.
                    if step == "session" {
                        entered.notify_one();
                        if ["release", "cancel", "never"].contains(&mode) {
                            release.notified().await;
                        }
                        if mode == "cancel" {
                            return Err("cancelled".into());
                        }
                    }
                    if mode == "both-errors" || mode == format!("{step}-error") {
                        return Err(format!("{step} failed"));
                    }
                    Ok(())
                })
            }),
            Arc::new(move |error| {
                reported
                    .lock()
                    .expect("#1071 cleanup warnings lock")
                    .push(error.into());
            }),
        )
        .await
        .expect("#1071 actual production assembly starts");
        // Timeout is the product's existing bounded cleanup policy. The outer
        // guard below only diagnoses a stuck test and never emits a receipt.
        // Only `never` exercises the deadline; the other modes get a budget
        // the test thread cannot miss under scheduler delay.
        let budget = if mode == "never" {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(5)
        };
        host.request_shutdown(budget);
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("#1071 production session disposal entered");
        if ["release", "cancel", "never"].contains(&mode) {
            negative_assert_eq!(
                calls.lock().expect("#1071 before cleanup calls").clone(),
                vec!["session"],
                "#1071 production cleanup waits for predecessor"
            );
            if mode != "never" {
                release.notify_one();
            }
        }
        let result = tokio::time::timeout(Duration::from_secs(5), host.stopped())
            .await
            .expect("#1071 production cleanup bounded test guard");
        let expected = match mode {
            "session-error" | "both-errors" => Err("session failed".to_owned()),
            "project-error" => Err("project failed".to_owned()),
            "cancel" => Err("cancelled".to_owned()),
            "never" => Err("gateway host drain deadline exceeded".to_owned()),
            _ => Ok(()),
        };
        let actual_calls = calls.lock().expect("#1071 completed cleanup calls").clone();
        let actual_warnings = warnings
            .lock()
            .expect("#1071 completed cleanup warnings")
            .clone();
        negative_assert_eq!(
            actual_calls,
            if mode == "never" {
                vec!["session"]
            } else {
                vec!["session", "project"]
            },
            "#1071 production cleanup attempts both resources"
        );
        negative_assert_eq!(
            result,
            expected,
            "#1071 production cleanup returns first error"
        );
        negative_assert_eq!(
            actual_warnings,
            if ["project-error", "both-errors"].contains(&mode) {
                vec!["project failed".to_owned()]
            } else {
                vec![]
            },
            "#1071 production cleanup reports project failure"
        );
        let fixture = oracle();
        let ts = fixture["observations"]["productionCleanup"]
            .as_array()
            .expect("#1071 TS production cleanup rows")
            .iter()
            .find(|row| row["mode"] == mode)
            .expect("#1071 TS cleanup mode");
        let ts_calls = if mode == "never" {
            &ts["before"]["calls"]
        } else {
            &ts["calls"]
        };
        negative_assert_eq!(
            json!(actual_calls),
            *ts_calls,
            "#1071 cleanup order matches TS probe"
        );
        println!(
            "#1071 cleanup {mode}: calls={actual_calls:?} result={result:?} project_diagnostics={actual_warnings:?}"
        );
    }
}

#[tokio::test]
async fn configured_hooks_are_refused() {
    let scratch = Scratch::new();
    let options = Options::parse(scratch.args()).expect("#1071 hooks options");
    let env = options.environment(Env::new());
    for scope in ["home", "project"] {
        std::fs::write(options.home.join("pilotdeck.yaml"), CONFIG)
            .expect("#1071 hooks base config");
        let base = if scope == "home" {
            options.home.clone()
        } else {
            options.project.join(".pilotdeck")
        };
        std::fs::create_dir_all(&base).expect("#1071 hooks config directory");
        let path = base.join("pilotdeck.yaml");
        std::fs::write(&path, format!("{CONFIG}hooks:\n  PreToolUse:\n    - hooks: [{{type: command, command: must-not-run}}]\n"))
            .expect("#1071 configured hook fixture");
        let error = crate::test_negative_receipt::expect_err(
            assembly::validate_profile(&options, &env).await,
            "#1071 configured hooks refused before readiness",
        );
        negative_assert!(
            error.to_string().contains("configured hooks"),
            "#1071 configured hooks refusal is explicit"
        );
        std::fs::remove_file(path).expect("#1071 remove hook config fixture");
    }
}

#[tokio::test]
async fn replacement_clone_shares_state() {
    use crate::runtime::{
        refresh::BoxFuture,
        replacement::{
            ReplacementCoordinator, ReplacementError, ReplacementSession, ReplacementStorage,
        },
    };
    let expected: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/native-cwd-marker/oracle.json"
    ))
    .expect("#1137 clone oracle JSON");
    let expected = &expected["observations"]["alias"];
    struct Idle;
    impl ReplacementSession for Idle {
        fn active_turn_run_id(&self, _: &JsString) -> Option<JsString> {
            None
        }
        fn has_active_turn(&self, _: &JsString) -> bool {
            false
        }
        fn abort_expected_turn(
            &self,
            _: &JsString,
            _: &JsString,
        ) -> BoxFuture<Result<(), ReplacementError>> {
            Box::pin(async { Ok(()) })
        }
        fn close_session(&self, _: &JsString) -> BoxFuture<Result<(), ReplacementError>> {
            Box::pin(async { Ok(()) })
        }
    }
    let original =
        ReplacementCoordinator::new(ReplacementStorage::default(), Arc::new(Idle), 1000, None);
    let cloned = original.clone();
    let session = JsString::from("clone-probe");
    original
        .reserve_transcript_write(&session, "probe")
        .expect("#1137 reserve through original clone");
    negative_assert_eq!(
        cloned.has_transcript_write_reservation(&session),
        expected["reservedThroughAlias"]
            .as_bool()
            .expect("#1137 TS alias reservation"),
        "#1137 replacement clone shares reservation"
    );
    let error = cloned
        .reserve_transcript_write(&session, "probe")
        .expect_err("#1137 cloned reservation must conflict");
    negative_assert_eq!(
        error.code,
        Some(
            expected["conflictCode"]
                .as_str()
                .expect("#1137 TS alias conflict")
        ),
        "#1137 replacement clone conflicts on same key"
    );
    cloned.release_transcript_write(&session);
    negative_assert_eq!(
        !original.has_transcript_write_reservation(&session),
        expected["releasedThroughOriginal"]
            .as_bool()
            .expect("#1137 TS alias release"),
        "#1137 replacement clone shares release"
    );
    println!(
        "#1137 clone probe: reserve original -> clone SESSION_BUSY -> release clone -> original unreserved"
    );
}

const MARKER_CASE_COUNT: usize = 15;

fn marker_oracle() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/native-cwd-marker/oracle.json"
    ))
    .expect("#1137 marker oracle JSON")
}

fn marker_bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).expect("#1137 marker hex is ASCII");
            u8::from_str_radix(text, 16).expect("#1137 marker hex byte")
        })
        .collect()
}

fn marker_snapshot(marker: &Path, project: &Path, other: &Path) -> Value {
    let entry = std::fs::symlink_metadata(marker).expect("#1137 marker node after registration");
    if entry.is_symlink() {
        json!({"kind": "symlink",
            "target": std::fs::read_link(marker).expect("#1137 marker symlink target"),
            "targetExists": marker.exists()})
    } else if entry.is_dir() {
        let mut entries = std::fs::read_dir(marker)
            .expect("#1137 marker directory contents")
            .map(|entry| {
                entry
                    .expect("#1137 marker directory entry")
                    .file_name()
                    .into_string()
                    .expect("#1137 marker directory entry UTF-8")
            })
            .collect::<Vec<_>>();
        entries.sort();
        json!({"kind": "directory", "entries": entries})
    } else {
        assert!(entry.is_file(), "#1137 marker regular file");
        let hex = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        let bytes = std::fs::read(marker).expect("#1137 read raw marker after registration");
        let bytes_hex = hex(&bytes)
            .replace(
                &hex(project.to_string_lossy().as_bytes()),
                &hex(b"{project}"),
            )
            .replace(&hex(other.to_string_lossy().as_bytes()), &hex(b"{other}"));
        json!({"kind": "file", "bytesHex": bytes_hex})
    }
}

#[test]
fn project_marker_ownership_matches_ts() {
    let expected = marker_oracle();
    let cases = expected["observations"]["cases"]
        .as_array()
        .expect("#1137 marker oracle cases");
    negative_assert!(!cases.is_empty(), "#1137 marker ownership cases nonempty");
    negative_assert_eq!(
        cases.len(),
        MARKER_CASE_COUNT,
        "#1137 marker ownership case count"
    );
    for row in cases {
        let root = std::env::temp_dir().join(format!("pdrs-1137-{}", uuid::Uuid::new_v4()));
        let mut scratch = Scratch(root);
        for name in ["home", "project", "other"] {
            std::fs::create_dir_all(scratch.0.join(name))
                .expect("#1137 marker scratch directories");
        }
        scratch.0 = scratch
            .0
            .canonicalize()
            .expect("#1137 canonical marker scratch");
        // Both bases are explicit: this matrix never reads global process cwd.
        let project = scratch.0.join("project");
        let other = scratch.0.join("other");
        let cwd = if row["cwd"] == "project" {
            &project
        } else {
            &other
        }
        .clone();
        let options = Options {
            home: scratch.0.join("home"),
            project,
            listen: "127.0.0.1:0".parse().expect("#1137 marker listen address"),
            token_file: scratch.0.join("unused-token"),
            sidecar: scratch.0.join("unused-sidecar"),
            sidecar_args: vec![],
            drain_budget: Duration::from_secs(1),
            storage_root: None,
        };
        let chats = crate::session_chat_dir(
            &options.home,
            &options.project,
            &options.project.to_string_lossy(),
        );
        let directory = chats.parent().expect("#1137 marker storage directory");
        std::fs::create_dir_all(directory).expect("#1137 create marker storage");
        let marker = directory.join(".cwd");
        match row["kind"].as_str().expect("#1137 marker fixture kind") {
            "directory" => std::fs::create_dir(&marker).expect("#1137 seed marker directory"),
            "symlink" => std::os::unix::fs::symlink("missing-target", &marker)
                .expect("#1137 seed dangling marker symlink"),
            "file" => {
                let bytes = if let Some(hex) = row["markerHex"].as_str() {
                    marker_bytes(hex)
                } else {
                    row["marker"]
                        .as_str()
                        .expect("#1137 marker fixture text")
                        .replace("{project}", &options.project.to_string_lossy())
                        .replace("{other}", &other.to_string_lossy())
                        .into_bytes()
                };
                std::fs::write(&marker, bytes).expect("#1137 seed raw marker");
            }
            _ => panic!("#1137 unknown marker fixture kind"),
        }
        let before = marker_snapshot(&marker, &options.project, &other);
        let raw_before = if row["kind"] == "file" {
            Some(std::fs::read(&marker).expect("#1137 raw marker before registration"))
        } else {
            None
        };
        negative_assert_eq!(
            before,
            row["before"],
            "#1137 marker initial node matches TS"
        );
        let accepted = row["registered"]
            .as_bool()
            .expect("#1137 TS registration result");
        let result = assembly::register_project_at(&options, &cwd);
        let after = marker_snapshot(&marker, &options.project, &other);
        let raw_unchanged = raw_before.is_none_or(|before| {
            before == std::fs::read(&marker).expect("#1137 raw marker after registration")
        });
        println!(
            "#1137 marker {}: registered={} error={:?} before={} after={}",
            row["name"],
            result.is_ok(),
            result.as_ref().err().map(|e| e.to_string()),
            before,
            after
        );
        negative_assert_eq!(
            result.is_ok(),
            accepted,
            "#1137 native marker ownership matches TS registration"
        );
        negative_assert_eq!(
            raw_unchanged && after == before,
            row["markerUnchanged"]
                .as_bool()
                .expect("#1137 TS marker preservation"),
            "#1137 existing marker bytes preserved"
        );
        negative_assert_eq!(after, row["after"], "#1137 marker final node matches TS");
    }
}

#[test]
fn marker_fixture_provenance() {
    let expected = marker_oracle();
    let cases = expected["observations"]["cases"]
        .as_array()
        .expect("#1137 provenance marker cases");
    negative_assert_eq!(
        cases.len(),
        MARKER_CASE_COUNT,
        "#1137 marker provenance case count"
    );
    let result = std::panic::catch_unwind(|| {
        crate::test_baseline::oracle(include_str!(
            "../../tests/fixtures/native-cwd-marker/oracle.json"
        ))
    });
    if let Err(payload) = result {
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("");
        if message.starts_with("#1109 oracle baseline commit mismatch:") {
            negative_assert!(false, "#1137 marker fixture fixed TS baseline");
        }
        std::panic::resume_unwind(payload);
    }
}

#[test]
#[ignore = "requires fixed read-only TS baseline and Node v22.22.0"]
fn marker_regenerates_identically() {
    let result = std::panic::catch_unwind(|| {
        crate::test_baseline::regenerates_identically("native-cwd-marker")
    });
    if let Err(payload) = result {
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("");
        if message.starts_with("#280 native-cwd-marker/oracle.json differs from the regenerated ") {
            negative_assert!(false, "#1137 marker oracle regenerated bytes");
        }
        std::panic::resume_unwind(payload);
    }
}

#[cfg(unix)]
fn path_scratch() -> Scratch {
    let mut scratch =
        Scratch(std::env::temp_dir().join(format!("pdrs-1137-path-{}", uuid::Uuid::new_v4())));
    for name in ["home", "project/child", "other", "deleted"] {
        std::fs::create_dir_all(scratch.0.join(name)).expect("#1137 path scratch directories");
    }
    for name in ["token", "sidecar"] {
        std::fs::write(scratch.0.join(name), "fixture").expect("#1137 path scratch files");
    }
    scratch.0 = scratch
        .0
        .canonicalize()
        .expect("#1137 path scratch canonical root");
    std::os::unix::fs::symlink(scratch.0.join("project"), scratch.0.join("alias"))
        .expect("#1137 path scratch symlink");
    scratch
}

#[cfg(unix)]
fn path_args(scratch: &Scratch, project: &str) -> Vec<OsString> {
    vec![
        "--home".into(),
        scratch.0.join("home").into_os_string(),
        "--project".into(),
        project.into(),
        "--listen".into(),
        "127.0.0.1:0".into(),
        "--token-file".into(),
        scratch.0.join("token").into_os_string(),
        "--sidecar".into(),
        scratch.0.join("sidecar").into_os_string(),
    ]
}

#[test]
#[cfg(unix)]
fn project_options_publish_and_restart_match_ts() {
    let expected = marker_oracle();
    let rows = expected["observations"]["paths"]["options"]
        .as_array()
        .expect("#1137 TS project option rows");
    negative_assert_eq!(rows.len(), 4, "#1137 project option matrix count");
    for row in rows {
        let scratch = path_scratch();
        let input = row["input"]
            .as_str()
            .expect("#1137 TS project option input");
        let options = crate::test_negative_receipt::expect_ok(
            Options::parse_at(path_args(&scratch, input), &scratch.0),
            "#1137 project options accept lexical paths",
        );
        negative_assert_eq!(
            options
                .project
                .to_string_lossy()
                .replace(&*scratch.0.to_string_lossy(), "{root}"),
            row["resolved"],
            "#1137 project identity matches TS lexical resolve"
        );
        let first = assembly::register_project_at(&options, &scratch.0);
        negative_assert!(first.is_ok(), "#1137 normalized project first publication");
        let chats = crate::session_chat_dir(
            &options.home,
            &options.project,
            &options.project.to_string_lossy(),
        );
        let marker = chats
            .parent()
            .expect("#1137 published marker parent")
            .join(".cwd");
        negative_assert_eq!(
            std::fs::read_to_string(&marker).expect("#1137 published marker bytes"),
            options.project.to_string_lossy(),
            "#1137 marker publishes normalized project identity"
        );
        // Parse again as a second startup would, then validate the already published marker.
        let restarted = crate::test_negative_receipt::expect_ok(
            Options::parse_at(path_args(&scratch, input), &scratch.0),
            "#1137 project options accept lexical paths",
        );
        negative_assert_eq!(
            assembly::register_project_at(&restarted, &scratch.0).is_ok(),
            row["registered"]
                .as_bool()
                .expect("#1137 TS restart result"),
            "#1137 second startup accepts its own marker"
        );
    }
    let scratch = path_scratch();
    std::os::unix::fs::symlink(scratch.0.join("home"), scratch.0.join("home-alias"))
        .expect("#1137 isolated home symlink");
    negative_assert!(
        Options::parse_at(path_args(&scratch, "home-alias"), &scratch.0).is_err(),
        "#1137 lexical project retains physical home separation"
    );
}

#[test]
#[cfg(unix)]
fn process_path_boundaries_match_ts() {
    // Only these dedicated ignored workers change cwd. The shared runner bounds
    // each child at 30 seconds and reaps it on timeout; no signal files are used.
    crate::test_process::run_cases(
        "native_process::tests::review::absolute_marker_deleted_cwd_worker",
        vec![("1137-deleted-cwd".into(), vec![])],
    );
    crate::test_process::run_cases(
        "native_process::tests::review::project_path_boundaries_worker",
        vec![("1137-project-paths".into(), vec![])],
    );
}

#[test]
#[cfg(unix)]
#[ignore = "isolated cwd worker; launched by process_path_boundaries_match_ts"]
fn absolute_marker_deleted_cwd_worker() {
    let scratch = path_scratch();
    let project = scratch.0.join("project").to_string_lossy().into_owned();
    let options = Options::parse_at(path_args(&scratch, &project), &scratch.0)
        .expect("#1137 deleted cwd options");
    assembly::register_project_at(&options, &scratch.0).expect("#1137 deleted cwd seed marker");
    let expected = marker_oracle();
    assert_eq!(
        expected["observations"]["paths"]["cwdUnreadable"], true,
        "#1137 TS deleted cwd really unreadable"
    );
    std::env::set_current_dir(scratch.0.join("deleted")).expect("#1137 enter doomed cwd");
    std::fs::remove_dir(scratch.0.join("deleted")).expect("#1137 delete cwd before product call");
    let parsed = Options::parse(path_args(&scratch, &project));
    let result = assembly::register_project(&options);
    // Restore to a known absolute directory, never reading the global cwd.
    std::env::set_current_dir(&scratch.0).expect("#1137 restore deleted cwd worker");
    negative_assert!(
        parsed.is_ok(),
        "#1137 absolute project parse ignores deleted cwd"
    );
    negative_assert_eq!(
        result.is_ok(),
        expected["observations"]["paths"]["deletedCwdAbsolute"]
            .as_bool()
            .expect("#1137 TS deleted cwd marker result"),
        "#1137 absolute marker ignores deleted cwd"
    );
    crate::test_process::receipt("1137-deleted-cwd");
}

#[cfg(unix)]
struct NoPathResources;
#[cfg(unix)]
impl crate::runtime::project::ProjectRuntimeFactory for NoPathResources {
    fn stage(
        &self,
        _: &Path,
        _: &mut crate::runtime::project::ResourceBundle,
    ) -> crate::runtime::project::RuntimeResult {
        panic!("#1137 path checks must not stage project resources")
    }
}
#[cfg(unix)]
impl crate::runtime::session::SessionFactory for NoPathResources {
    fn create(
        &self,
        _: crate::runtime::session::SessionContext,
        _: Option<crate::runtime::session::LiveSessionHandle>,
    ) -> crate::runtime::refresh::BoxFuture<
        Result<crate::runtime::session::LiveSessionHandle, crate::runtime::project::RuntimeError>,
    > {
        panic!("#1137 path checks must not create a session")
    }
}

#[tokio::test]
#[cfg(unix)]
#[ignore = "isolated cwd worker; launched by process_path_boundaries_match_ts"]
async fn project_path_boundaries_worker() {
    use crate::runtime::{
        project::ProjectRuntimeRegistry,
        session::{SessionOwner, SessionOwnerOptions},
        turn::{gateway::GatewayRequests, submit_host::SubmitHostServices},
    };
    let scratch = path_scratch();
    let options = Options::parse_at(path_args(&scratch, "project"), &scratch.0)
        .expect("#1137 path boundary options");
    std::fs::write(options.home.join("pilotdeck.yaml"), CONFIG)
        .expect("#1137 path boundary config");
    assembly::register_project_at(&options, &scratch.0).expect("#1137 path boundary marker");
    let mut owner_options = SessionOwnerOptions::new(Arc::new(NoPathResources));
    owner_options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(owner_options);
    let requests = requests::Requests {
        project: Arc::new(ProjectRuntimeRegistry::new(
            options.project.clone(),
            Arc::new(NoPathResources),
        )),
        options: options.clone(),
        env: options.environment(Env::new()),
        snapshot_enabled: false,
        memory: Arc::new(crate::runtime::memory::gateway::MemoryGateway::disabled()),
        coordinator: Arc::default(),
        runs: Some(Arc::new(
            crate::runtime::turn::publish::FileRegistry::new(
                crate::storage::FileRunRegistry::open(options.home.join("runs.json"))
                    .expect("#1137 path run file"),
            )
            .expect("#1137 path run registry"),
        )),
        owner: owner.clone(),
        replacement: Arc::new(crate::runtime::replacement::ReplacementCoordinator::new(
            Default::default(),
            Arc::new(owner.clone()),
            5000,
            None,
        )),
        recorders: Arc::default(),
        interactions: Arc::default(),
        mcp: requests::Requests::stub_mcp(&owner),
        async_hook_registry: None,
        user_dialog_bus: Arc::new(std::sync::Mutex::new(
            crate::user_dialog::GatewayUserDialogBus::new(
                crate::user_dialog::DialogRuntime::default(),
                None,
            ),
        )),
        upload_gateway: Arc::new(crate::upload::gateway::UploadGateway::unavailable()),
    };
    let services = services::Services {
        options: options.clone(),
        env: options.environment(Env::new()),
        project: Arc::new(ProjectRuntimeRegistry::new(
            options.project.clone(),
            Arc::new(NoPathResources),
        )),
        recorders: Arc::default(),
        owner,
        snapshot_recorder: None,
        upload_store: Arc::new(crate::upload::UploadStore::new(
            crate::upload::UploadStoreOptions::new(
                |_: &pilotdeck_protocol::JsString| {
                    Err(crate::upload::UploadError::dialog(
                        "PROJECT_NOT_FOUND",
                        "unavailable in test",
                    ))
                },
                || Ok(vec![]),
            ),
        )),
    };
    std::env::set_current_dir(scratch.0.join("other")).expect("#1137 enter other cwd");
    let oracle = marker_oracle();
    let paths = &oracle["observations"]["paths"];
    let rows = paths["boundaries"]
        .as_array()
        .expect("#1137 TS boundary rows");
    negative_assert_eq!(rows.len(), 5, "#1137 project boundary matrix count");
    for row in rows {
        let input = row["input"]
            .as_str()
            .expect("#1137 boundary input")
            .replace("{root}", &scratch.0.to_string_lossy());
        let params = pilotdeck_protocol::parse_json(
            &json!({"projectKey":input,"sessionKey":"s"}).to_string(),
        )
        .expect("#1137 path parameters JSON");
        let read = requests
            .request("active_turn_snapshot".into(), params.clone())
            .await;
        negative_assert_eq!(
            read.is_ok(),
            row["registered"]
                .as_bool()
                .expect("#1137 TS boundary registration"),
            "#1137 request project guard uses process cwd"
        );
        negative_assert_eq!(
            services.workspace_id(&params).is_ok(),
            row["storageMatches"]
                .as_bool()
                .expect("#1137 TS storage boundary"),
            "#1137 submit project guard uses process cwd"
        );
    }
    std::env::set_current_dir(&scratch.0).expect("#1137 restore path boundary worker");
    services
        .project
        .dispose()
        .await
        .expect("#1137 unused path project disposed");
    crate::test_process::receipt("1137-project-paths");
}

// #1316: the native data dir has no TS session writers. Durable session
// writes (a status entry plus rename/tag metadata records) served by the
// production assembly happen inside this process: the child-process set is
// the same before and after, no node executable appears among the
// children, and no other process holds an fd into the scratch data dir.
// Like the #1349 audit, the check runs in an owned rerun of this test
// binary because this lib shares one process with sibling tests that do
// spawn children, which would make the before/after comparison race on
// their activity instead of on this audit's writes.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn session_writes_have_no_ts_writers() {
    if std::env::var_os("PDRS_NATIVE_SESSION_AUDIT_CHILD").is_none() {
        // #1632: bounded, file-backed, own process group. `Command::output()`
        // waited for the pipe write ends, so the very regression this audit
        // detects (a long-lived child inheriting stdout) blocked the parent
        // until the CI job timed out and the failure evidence was lost.
        let rerun = crate::test_process::rerun_exact(
            "native_process::tests::review::session_writes_have_no_ts_writers",
            &[("PDRS_NATIVE_SESSION_AUDIT_CHILD", "1")],
            crate::test_process::RERUN_LIMIT,
        );
        assert!(
            !rerun.timed_out,
            "#1316 the audit rerun exceeded its deadline:\n{}\n{}",
            rerun.stdout, rerun.stderr
        );
        assert!(
            crate::test_exact_receipt::single_pass(
                &rerun.status,
                "native_process::tests::review::session_writes_have_no_ts_writers",
                &rerun.stdout
            ),
            "#1316 the audit rerun matched no test:\n{}\n{}",
            rerun.stdout,
            rerun.stderr
        );
        return;
    }
    let (_scratch, requests) =
        std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1316 the audit fixture");
    let root = requests
        .options
        .home
        .parent()
        .expect("#1316 the audit scratch root")
        .to_owned();
    let transcript = crate::session_chat_dir(
        &requests.options.home,
        &requests.options.project,
        &requests.options.project.to_string_lossy(),
    )
    .join("a8-session.jsonl");
    let before = std::fs::read_to_string(&transcript).expect("#1316 seeded transcript");
    // Sample `/proc` for the whole write window, not only before and after it:
    // a child or fd holder that appears and exits between two snapshots would
    // otherwise leave the audit green (false PASS).
    // The before-set is the sampler's first sweep, taken when the window
    // opens; a snapshot after `start` would miss a pre-window child that exits
    // in the gap and report it as transient (#1632).
    let (audit, children_before) = TransientAudit::start(root);
    let status = crate::runtime::turn::gateway::GatewayRequests::request(
        &requests,
        "record_agent_status_message".into(),
        crate::metadata::object([
            ("sessionKey", "a8-session".into()),
            ("turnId", "audit-status".into()),
            (
                "status",
                crate::metadata::object([("subtype", "audit".into())]),
            ),
        ]),
    )
    .await
    .expect("#1316 the status write resolves");
    negative_assert_eq!(
        status
            .get("recorded")
            .and_then(pilotdeck_protocol::JsValue::as_bool),
        Some(true),
        "#1316 the cold-path status write is recorded"
    );
    for (method, value) in [
        ("rename_session", "audit-rename"),
        ("tag_session", "audit-tag"),
    ] {
        let updated = crate::runtime::turn::gateway::GatewayRequests::request(
            &requests,
            method.into(),
            crate::metadata::object([("sessionKey", "a8-session".into()), ("value", value.into())]),
        )
        .await
        .unwrap_or_else(|_| panic!("#1316 {method} resolves"));
        negative_assert_eq!(
            updated
                .get("updated")
                .and_then(pilotdeck_protocol::JsValue::as_bool),
            Some(true),
            "#1316 {method} writes metadata"
        );
    }
    // The guarded writes are over: stop the sampler and collect everything it
    // ever observed inside the window.
    let (children_seen, holders_seen) = audit.finish();
    let after = std::fs::read_to_string(&transcript).expect("#1316 audited transcript");
    negative_assert!(
        after.len() > before.len(),
        "#1316 the audited writes reached the native data dir"
    );
    for marker in ["audit-status", "sdk-rename", "sdk-tag"] {
        negative_assert!(
            after.contains(marker),
            "#1316 the transcript carries {marker}"
        );
    }
    let children_after = child_processes();
    negative_assert_eq!(
        children_after,
        children_before,
        "#1316 session writes spawn no child processes"
    );
    // The before/after snapshots cannot see a child that lived only inside the
    // window; the continuous sampler keeps every pid it ever observed.
    let transient: Vec<String> = children_seen
        .iter()
        .filter(|(pid, _)| !children_before.iter().any(|(before, _)| before == *pid))
        .map(|(pid, exe)| format!("{pid}:{exe:?}"))
        .collect();
    negative_assert!(
        transient.is_empty(),
        "#1316 session writes spawn no child processes, even transiently ({transient:?})"
    );
    for (pid, exe) in &children_seen {
        let exe = exe
            .as_ref()
            .map(|exe| exe.to_string_lossy().into_owned())
            .unwrap_or_default();
        negative_assert!(
            !exe.contains("node"),
            "#1316 no node executable among the children (pid {pid}: {exe})"
        );
    }
    negative_assert!(
        holders_seen.is_empty(),
        "#1316 no other process holds the native data dir open, even transiently ({holders_seen:?})"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1316 the audit owner shuts down");
    requests
        .project
        .dispose()
        .await
        .expect("#1316 the audit project disposes");
}

/// #1316: the `/proc` audit is Linux-only; elsewhere it is skipped with this
/// note, like the other parity audits of this crate.
#[cfg(not(target_os = "linux"))]
#[test]
fn session_writer_audit_is_linux_only() {
    eprintln!("SKIP: #1316 the session-writer audit reads Linux /proc");
}

/// #1632: a child that was already alive when the write window opened must not
/// be reported as transient just because it exited before the old post-`start`
/// snapshot. The pre-fix ordering (`start`, then `child_processes()`) made that
/// a false FAIL; the window's before-set is now the first sweep itself.
#[cfg(target_os = "linux")]
#[test]
fn transient_audit_before_set_opens_with_the_window() {
    let scratch = crate::test_scratch::Scratch::new("1632-audit");
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("#1632 the pre-window child starts");
    let pid = child.id() as i32;
    let (audit, children_before) = TransientAudit::start(scratch.path().to_path_buf());
    negative_assert!(
        children_before.contains_key(&pid),
        "#1632 the window's before-set carries the child alive when it opened"
    );
    // The pre-window child exits right after the window opens. The old code
    // took its "before" snapshot here, so the sampler had the child but the
    // snapshot did not.
    child.kill().expect("#1632 stop the pre-window child");
    child.wait().expect("#1632 reap the pre-window child");
    negative_assert!(
        !child_processes()
            .iter()
            .any(|(observed, _)| *observed == pid),
        "#1632 the child is gone before the old post-start snapshot would run"
    );
    let (children_seen, _holders) = audit.finish();
    let transient: Vec<String> = children_seen
        .iter()
        .filter(|(observed, _)| !children_before.contains_key(observed))
        .map(|(observed, exe)| format!("{observed}:{exe:?}"))
        .collect();
    negative_assert!(
        transient.is_empty(),
        "#1632 a pre-window child is not transient ({transient:?})"
    );
}

#[cfg(target_os = "linux")]
fn child_processes() -> Vec<(i32, Option<std::path::PathBuf>)> {
    let me = std::process::id() as i32;
    let mut children = Vec::new();
    let entries = std::fs::read_dir("/proc").expect("#1316 the audit reads /proc");
    for entry in entries {
        let entry = entry.expect("#1316 the audit reads a /proc entry");
        let Some(pid) = entry.file_name().to_string_lossy().parse::<i32>().ok() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        // After `comm)` come state and ppid.
        let Some(ppid) = rest
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<i32>().ok())
        else {
            continue;
        };
        if ppid == me {
            let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
            children.push((pid, exe));
        }
    }
    children.sort();
    children
}

#[cfg(target_os = "linux")]
fn foreign_data_dir_holders(root: &std::path::Path) -> Vec<i32> {
    let me = std::process::id() as i32;
    let mut holders = Vec::new();
    let entries = std::fs::read_dir("/proc").expect("#1316 the audit reads /proc");
    for entry in entries {
        let entry = entry.expect("#1316 the audit reads a /proc entry");
        let Some(pid) = entry.file_name().to_string_lossy().parse::<i32>().ok() else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for fd in fds {
            let Ok(fd) = fd else {
                continue;
            };
            if let Ok(target) = std::fs::read_link(fd.path())
                && target.starts_with(root)
            {
                holders.push(pid);
                break;
            }
        }
    }
    holders.sort();
    holders
}

/// Everything the sampler observed inside the write window. A named alias keeps
/// the `JoinHandle` field and the `finish` return type under
/// `clippy::type_complexity` (CI runs clippy with `-D warnings`).
#[cfg(target_os = "linux")]
type TransientObservations = (TransientChildren, std::collections::BTreeSet<i32>);

/// The child set of one `/proc` sweep, keyed by pid.
#[cfg(target_os = "linux")]
type TransientChildren = std::collections::BTreeMap<i32, Option<std::path::PathBuf>>;

/// Poll `/proc` for the whole audited write window.
///
/// Two snapshots (before/after) say nothing about a child or fd holder whose
/// lifetime is shorter than the gap between them, so a transient writer would
/// slip through both and leave the audit green. This sampler keeps running
/// until `finish`, accumulating every child pid and every foreign fd holder it
/// ever observes; the assertions then judge the accumulated sets.
///
/// Coverage is bracketed on both ends, not assumed: `start` returns only after
/// the first full sweep has completed, together with that sweep's child set
/// (the window's "before" snapshot, see [`TransientAudit::start`]), and
/// `finish` sets the stop flag *before*
/// the worker takes its last sweep (`done` is read at the top of the cycle, so a
/// stop request during a sweep still buys one more), which means the worker
/// always sweeps once after the last write. The residual blind spot is the gap
/// between two consecutive sweeps — one `/proc` pass pair plus the 1 ms sleep,
/// i.e. proportional to the host's process and fd count, not 1 ms. A writer
/// whose entire lifetime fits inside one such gap is invisible to this audit.
#[cfg(target_os = "linux")]
struct TransientAudit {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<TransientObservations>>,
}

#[cfg(target_os = "linux")]
impl TransientAudit {
    /// Open the sampling window and return the first sweep's child set.
    ///
    /// The "before" snapshot and the opening of the window must be the same
    /// observation. Taking `child_processes()` *after* `start` returns leaves a
    /// gap in which a child that already existed when the window opened can
    /// exit; the sampler has it in `children_seen` but the late snapshot does
    /// not, so the audit reports it as a transient writer and fails a run in
    /// which nothing regressed (#1632). Returning the first sweep removes that
    /// gap: the returned set is exactly what the sampler saw when the window
    /// opened.
    fn start(root: std::path::PathBuf) -> (Self, TransientChildren) {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        // `start` must not hand control back before the first sweep has run:
        // otherwise the write could begin during the thread's scheduling delay
        // and the opening of the window would be unsampled.
        let (swept, first_sweep) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut children = TransientChildren::new();
            let mut holders = std::collections::BTreeSet::new();
            let mut announced = false;
            loop {
                // Read the flag before sweeping: a stop request that lands while
                // this sweep runs still leaves one more sweep after the writes.
                let done = flag.load(std::sync::atomic::Ordering::SeqCst);
                for (pid, exe) in child_processes() {
                    children.insert(pid, exe);
                }
                for pid in foreign_data_dir_holders(&root) {
                    holders.insert(pid);
                }
                if !announced {
                    announced = true;
                    // The first sweep is the window's opening snapshot. A
                    // closed receiver only means `start` was abandoned; keep
                    // sampling, `Drop` still stops this thread.
                    let _ = swept.send(children.clone());
                }
                if done {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            (children, holders)
        });
        let children_before = first_sweep
            .recv()
            .expect("#1316 the audit sampler completes its first sweep");
        (
            Self {
                stop,
                worker: Some(worker),
            },
            children_before,
        )
    }

    fn finish(mut self) -> TransientObservations {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.worker
            .take()
            .expect("#1316 the audit sampler is running")
            .join()
            .expect("#1316 the audit sampler joins")
    }
}

/// A panic before `finish` must not leave a spinning sampler in the test
/// process; stopping it here also keeps the owned rerun process bounded.
#[cfg(target_os = "linux")]
impl Drop for TransientAudit {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
