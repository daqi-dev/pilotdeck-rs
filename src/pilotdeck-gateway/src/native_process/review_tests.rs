use super::*;
// The `/proc` audit toolkit lives in the crate's shared test-support module so
// the #1349 module-host audit uses the same predicate and the same sampler
// (#1632); it used to be a private copy here.
#[cfg(target_os = "linux")]
use crate::test_process::{TransientAudit, TransientChildren, child_processes};

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
        tools::SessionMcpTools::empty(),
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

/// #1864 (b): the host-side permission *contexts* — the admission
/// `PermissionContext`, the emitted `permissionContext` and the turn config's
/// `permissionMode` (the copy `tool_context` and `resolve_workspace_path`
/// read) — follow the store `set_permission_mode` wrote, on every turn, with
/// the same store, key and priority the prepare closure and the live
/// adapter's per-turn source use (`managedPermissions.defaultMode` still
/// wins), so a turn never runs one mode for the sidecar and another for
/// native tool admission. What is pinned is the contexts, not the
/// decisions: the registry only carries read-only tools, which `decide`
/// allows under all three modes.
#[test]
fn session_tools_admission_mode_follows_the_store_each_turn() {
    use crate::host::modules::tool_context::HostToolCheckpoint;
    use crate::runtime::{
        hooks::Cancellation,
        session::{SessionContext, factory::SessionBinding},
    };
    use pilotdeck_protocol::admission::ExecutionIdentity;

    let scratch = Scratch::new();
    let options = Options::parse(scratch.args()).expect("#1864 admission options");
    let recorder = crate::runtime::turn::live::test_recorder();
    let overrides = Arc::new(SessionOverrides::default());
    let key: JsString = "session".into();
    let identity = ExecutionIdentity {
        run_id: "run".into(),
        operation_id: "operation".into(),
        request_id: "request".into(),
        session_id: "session".into(),
        turn_id: "turn".into(),
    };
    let binding = || SessionBinding {
        context: SessionContext {
            session_key: "session".into(),
            project_key: None,
            channel_key: "probe".into(),
            allowed_tools: None,
            disallowed_tools: None,
        },
        recorder: recorder.clone(),
        checkpoint: HostToolCheckpoint::new(None, &[]).expect("#1864 admission checkpoint"),
        projection: Arc::default(),
    };
    let mut config = JsObject::new();
    config.insert("permissionMode".into(), JsValue::String("default".into()));
    let tools = tools::SessionTools::new(
        &options,
        &Env::new(),
        binding(),
        config,
        Arc::default(),
        Arc::default(),
        tools::SessionMcpTools::empty(),
    )
    .expect("#1864 admission session tools")
    .with_permission_mode_source(Arc::new({
        let overrides = overrides.clone();
        let key = key.clone();
        move || overrides.permission_mode(&key)
    }));
    let admission_context_mode = |ports: &crate::runtime::turn::host_factory::SessionPorts| {
        ports
            .interaction
            .permission
            .context
            .mode
            .as_js_string()
            .clone()
    };
    let emitted_mode = |ports: &crate::runtime::turn::host_factory::SessionPorts| {
        ports
            .context_config
            .get("permissionContext")
            .and_then(|value| value.get("mode"))
            .and_then(JsValue::as_string)
            .cloned()
    };
    let turn_config_mode = |ports: &crate::runtime::turn::host_factory::SessionPorts| {
        ports
            .context_config
            .get("permissionMode")
            .and_then(JsValue::as_string)
            .cloned()
    };
    let ports = assembly::turn_ports(&tools, &identity, recorder.clone(), Cancellation::new());
    negative_assert_eq!(
        admission_context_mode(&ports),
        JsString::from("default"),
        "#1864 no stored selection keeps the default admission mode"
    );
    negative_assert_eq!(
        emitted_mode(&ports),
        Some(JsString::from("default")),
        "#1864 the emitted permissionContext carries the default mode"
    );
    negative_assert_eq!(
        turn_config_mode(&ports),
        Some(JsString::from("default")),
        "#1864 the turn config carries the default admission mode"
    );
    // The selector's write: the next turn's admission context, emitted
    // permissionContext and turn config all carry the stored mode.
    overrides.set_permission_mode(&key, "plan".into());
    let ports = assembly::turn_ports(&tools, &identity, recorder.clone(), Cancellation::new());
    negative_assert_eq!(
        admission_context_mode(&ports),
        JsString::from("plan"),
        "#1864 the stored mode reaches the host admission context"
    );
    negative_assert_eq!(
        emitted_mode(&ports),
        Some(JsString::from("plan")),
        "#1864 the emitted permissionContext carries the stored mode"
    );
    negative_assert_eq!(
        turn_config_mode(&ports),
        Some(JsString::from("plan")),
        "#1864 the stored mode reaches the turn config permissionMode"
    );
    // A non-empty managedPermissions.defaultMode wins over the store.
    let mut managed = JsObject::new();
    managed.insert(
        "defaultMode".into(),
        JsValue::String("bypassPermissions".into()),
    );
    let mut guarded = JsObject::new();
    guarded.insert(
        "permissionMode".into(),
        JsValue::String("bypassPermissions".into()),
    );
    guarded.insert("managedPermissions".into(), JsValue::Object(managed));
    let tools = tools::SessionTools::new(
        &options,
        &Env::new(),
        binding(),
        guarded,
        Arc::default(),
        Arc::default(),
        tools::SessionMcpTools::empty(),
    )
    .expect("#1864 guarded session tools")
    .with_permission_mode_source(Arc::new({
        let overrides = overrides.clone();
        let key = key.clone();
        move || overrides.permission_mode(&key)
    }));
    let ports = assembly::turn_ports(&tools, &identity, recorder, Cancellation::new());
    negative_assert_eq!(
        admission_context_mode(&ports),
        JsString::from("bypassPermissions"),
        "#1864 a non-empty managed defaultMode wins over the stored mode"
    );
    negative_assert_eq!(
        turn_config_mode(&ports),
        Some(JsString::from("bypassPermissions")),
        "#1864 a non-empty managed defaultMode wins in the turn config too"
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
                vec!["automation", "channels", "session"],
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
                // #1328: the channel assembly stops before the gateway
                // lifecycle disposal (PilotDeckServerShutdownBundle.ts:33-37).
                vec!["automation", "channels", "session"]
            } else {
                vec!["automation", "channels", "session", "project"]
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
        // #1328: the native host stops its channel assembly before the gateway
        // lifecycle disposal (PilotDeckServerShutdownBundle.ts:33-37). The TS
        // probe (tests/fixtures/native-process/generate.mts) has no observer
        // for that step, so its `calls` only carry the gateway disposals: the
        // channels step is pinned here, on the Rust side, and dropped before
        // the golden comparison. Making the generator emit a synthetic
        // "channels" entry would turn the fixture into a mirror of this
        // implementation instead of a reading of the baseline. #2294: the
        // automation bundle now stops ahead of the channels
        // (PilotDeckServerShutdownBundle.ts:32) for the same reason — the
        // probe observes neither — so both native-only steps are pinned here
        // and dropped together.
        negative_assert_eq!(
            actual_calls.first().copied(),
            Some("automation"),
            "#2294 automation bundle stops before the channel assembly"
        );
        negative_assert_eq!(
            actual_calls.get(1).copied(),
            Some("channels"),
            "#1328 channel assembly stops before the gateway lifecycle disposal"
        );
        // #1725: consume the pinned native-only prefix positionally —
        // dropping the steps by value would silently swallow an automation
        // or channels step that landed anywhere later in the order too,
        // instead of only the first two the assertions above pinned.
        let gateway_calls: Vec<&str> = actual_calls.iter().copied().skip(2).collect();
        negative_assert_eq!(
            json!(gateway_calls),
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
        // #1600: match production (connected), not the retired
        // observer-coupled bus.
        user_dialog_bus: Arc::new(std::sync::Mutex::new(
            crate::user_dialog::GatewayUserDialogBus::connected(
                crate::user_dialog::DialogRuntime::default(),
            ),
        )),
        upload_gateway: Arc::new(crate::upload::gateway::UploadGateway::unavailable()),
        prepare_weixin_login: None,
        cron: None,
        always_on: None,
        settings: requests::Requests::stub_settings(&owner),
        session_overrides: Arc::default(),
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
// production assembly happen inside this process: the window adds no child
// process (the end snapshot is a subset of the before set — an exit is not a
// spawn), no node executable appears among the children at any point in the
// window, and no other process holds an fd into the scratch data dir.
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
        crate::test_process::rerun_audit_case(
            "native_process::tests::review::session_writes_have_no_ts_writers",
            &[("PDRS_NATIVE_SESSION_AUDIT_CHILD", "1")],
            "#1316",
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
    // One shared verdict for both audits: end snapshot adds nothing, nothing
    // appeared even transiently (the before/after snapshots cannot see a child
    // that lived only inside the window; the continuous sampler can), and no
    // child runs node. The predicates live in `test_process` together so the
    // #1349 audit cannot drift from this one.
    if let Some(failure) = crate::test_process::child_spawn_failure(
        "#1316",
        "session writes",
        &children_seen,
        &children_before,
    ) {
        negative_assert!(false, "{failure}");
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
    // #1900: the print-family macros are captured by the test harness, so on
    // the hosts where this test exists the note would never reach the log. The
    // shared writer goes straight to the stderr handle, like the module_host
    // sibling. No nextest success-output entry: the test does not exist in the
    // Linux CI job at all.
    crate::test_skip_line::stderr_skip_line(
        "SKIP: #1316 the session-writer audit reads Linux /proc\n",
    );
}

/// #1632: a child that was already alive when the write window opened must not
/// be reported as transient just because it exited before the old post-`start`
/// snapshot. The pre-fix ordering (`start`, then `child_processes()`) made that
/// a false FAIL; the window's before-set is now the first sweep itself.
#[cfg(target_os = "linux")]
#[test]
fn transient_audit_before_set_opens_with_the_window() {
    let scratch = crate::test_scratch::Scratch::new("1632-audit");
    // Kill-on-drop: a panic between the spawn and the explicit kill must not
    // leave a `sleep` holding this process's stdout/stderr — that is exactly
    // the failure mode #1632 removes, and it would hang `cargo test`'s pipe.
    let mut child = crate::test_process::Worker(
        std::process::Command::new("sleep")
            .arg("30")
            // No inherited streams: if this binary is killed outright the
            // probe must not keep `cargo test`'s pipe open — the same hazard
            // `rerun_exact` removes for the audit rerun (r4 concurrency).
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("#1632 the pre-window child starts"),
    );
    let pid = child.0.id() as i32;
    let (audit, children_before) = TransientAudit::start(scratch.path().to_path_buf());
    negative_assert!(
        children_before.contains_key(&pid),
        "#1632 the window's before-set carries the child alive when it opened"
    );
    // The pre-window child exits right after the window opens. The old code
    // took its "before" snapshot here, so the sampler had the child but the
    // snapshot did not.
    child.0.kill().expect("#1632 stop the pre-window child");
    child.0.wait().expect("#1632 reap the pre-window child");
    negative_assert!(
        !child_processes()
            .iter()
            .any(|(observed, _)| *observed == pid),
        "#1632 the child is gone before the old post-start snapshot would run"
    );
    // The pre-fix ordering snapshotted the children here, i.e. without the
    // already-exited pre-window child, while the sampler still had it: that
    // before-set reports the child as transient, the false FAIL this guards.
    // #1723: the value grew a reuse record alongside the exe; a bare first
    // observation is what a snapshot holds.
    let old_before: TransientChildren = child_processes()
        .into_iter()
        .map(|(pid, exe)| (pid, crate::test_process::ObservedChild::first(exe)))
        .collect();
    let (children_seen, _holders) = audit.finish();
    // Deliberately the *pre-fix* computation, kept as the demonstration that
    // the old ordering reports this child; the live predicate below is the
    // shared one.
    let old_transient: Vec<i32> = children_seen
        .iter()
        .filter(|(observed, _)| !old_before.contains_key(observed))
        .map(|(observed, _)| *observed)
        .collect();
    negative_assert!(
        old_transient.contains(&pid),
        "#1632 the pre-fix before-set reports the pre-window child ({old_transient:?})"
    );
    println!(
        "PDRS_AUDIT_WINDOW pid={pid} before={} old_before_has_pid={} old_transient={old_transient:?}",
        children_before.len(),
        old_before.contains_key(&pid)
    );
    // With the window's own before-set the same observation is not transient.
    // Other tests of this binary spawn children concurrently, so only this
    // test's own pre-window child is asserted on.
    let transient = crate::test_process::unknown_children(&children_seen, &children_before);
    negative_assert!(
        !transient
            .iter()
            .any(|entry| entry.starts_with(&format!("{pid}:"))),
        "#1632 the pre-window child is not transient ({transient:?})"
    );
    // The other consumer of the same before-set: the end snapshot must not be
    // required to equal it. This test's pre-window child is exactly the input
    // that makes the old equality fail; the subset rule the audit now uses
    // accepts it, because an exit is not a spawn.
    let children_after = crate::test_process::end_snapshot(&children_seen);
    println!(
        "PDRS_AUDIT_END pid={pid} in_before={} in_after={} (old equality needs both)",
        children_before.contains_key(&pid),
        children_after.contains_key(&pid)
    );
    negative_assert!(
        !children_after.contains_key(&pid),
        "#1632 the pre-window child is gone by the end snapshot"
    );
}

use crate::config::permission::PermissionRule;
use serde_json::{Value, json};

fn ask_tool_names(rules: &crate::config::permission::Lease) -> Vec<String> {
    rules
        .rules()
        .snapshot()
        .ask
        .iter()
        .map(|rule| String::from_utf16_lossy(rule.tool_name.units()))
        .collect()
}

/// Raw outbound frames. Nothing here parses them: outbound JSON may carry a
/// lone surrogate that `serde_json` rejects (`runtime/connection/mod.rs`), and a
/// panic inside the writer task would poison the lock and surface as an
/// unrelated assertion failure.
#[derive(Default)]
struct Frames(Mutex<Vec<String>>);
impl crate::runtime::connection::GatewayTextConnection for Frames {
    fn send_text(&self, text: String) {
        self.0.lock().expect("#1429 frames lock").push(text);
    }
    fn close(&self, _: u16, _: &str) {}
}

/// #1429: `syncSdkMcpPermissionRules` also runs when a session lease is
/// acquired (`ProjectRuntimeRegistry.ts:305-308`). Nothing pinned that: the
/// acquire harnesses built their config and acquired without ever syncing, and
/// an unpublished `OnceLock` skipped the sync silently, so deleting the call
/// (or losing the publication it depends on) left every ruler green.
///
/// The fixture makes the acquire path the only possible source of the rule:
/// the production MCP override route seeds `mcp__a__*` into the live ask
/// array, the test then empties that array, and only the session acquire that
/// follows can put the rule back.
#[tokio::test]
async fn acquiring_a_session_syncs_the_mcp_ask_rules() {
    let scratch = Scratch::new();
    let options = Options::parse(scratch.args()).expect("#1429 options");
    std::fs::write(options.home.join("pilotdeck.yaml"), CONFIG).expect("#1429 config");
    let key = JsString::from("session");
    let registry: Arc<crate::config::permission::GatewaySessionPermissionRuleSetRegistry> =
        Arc::default();
    // A lease of our own keeps the session's rule-set entry (and its live ask
    // array) alive after the turn that follows the assertion point.
    let probe = registry.acquire(&key, None);
    let host = assembly::start_observed_with_permission_rule_sets(
        options.clone(),
        options.environment(Env::new()),
        registry.clone(),
        Arc::new(|message| panic!("#1429 the published config never misses: {message}")),
        true,
        Arc::new(|_, result| Box::pin(async move { result })),
        Arc::new(|_| {}),
    )
    .await
    .expect("#1429 observed assembly starts");
    let frames = Arc::new(Frames::default());
    let connection = host.connect(frames.clone(), "token".into(), "fixture".into());
    connection
        .receive(
            json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"fixture"})
                .to_string(),
        )
        .await
        .expect("#1429 hello");
    // Seed the generated rule through the production route, so the assertion
    // below cannot be satisfied by a hand-built rule object.
    connection
        .receive(
            json!({"type":"request","id":"override","method":"set_mcp_permission_mode_override","params":{"sessionKey":"session","serverName":"a","mode":"auto"}})
                .to_string(),
        )
        .await
        .expect("#1429 MCP override route");
    negative_assert_eq!(
        ask_tool_names(&probe),
        vec!["mcp__a__*".to_owned()],
        "#1429 the override route seeds the generated ask rule"
    );
    // Take it away again: from here only the acquire path can restore it.
    probe.rules().member("ask").update_array(|ask| ask.clear());
    negative_assert!(
        ask_tool_names(&probe).is_empty(),
        "#1429 the fixture's live ask array starts empty"
    );
    let submit = connection.clone();
    let turn = tokio::spawn(async move {
        // The turn itself is allowed to fail (the fixture model URL refuses
        // connections); this test only needs the session acquire inside it.
        let _ = submit
            .receive(
                json!({"type":"request","id":"turn","method":"submit_turn","params":{"sessionKey":"session","runId":"run","message":"hello","channelKey":"test"}})
                    .to_string(),
            )
            .await;
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while ask_tool_names(&probe).is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let ask = probe.rules().snapshot().ask;
    negative_assert_eq!(
        ask.len(),
        1,
        "#1429 the acquire path restores the generated ask rule"
    );
    let rule = &ask[0];
    negative_assert!(
        rule.source.equals_str("session"),
        "#1429 the synced rule keeps the generated source"
    );
    negative_assert!(
        rule.behavior.equals_str("ask"),
        "#1429 the synced rule keeps the ask behavior"
    );
    negative_assert!(
        rule.tool_name.equals_str("mcp__a__*"),
        "#1429 the synced rule is the MCP-generated wildcard for server a"
    );
    negative_assert_eq!(
        rule.force,
        Some(true),
        "#1429 the synced rule keeps force: true"
    );
    negative_assert!(
        rule.pattern.is_none(),
        "#1429 the synced rule carries no pattern"
    );
    turn.abort();
    host.request_shutdown(options.drain_budget);
    let _ = tokio::time::timeout(Duration::from_secs(10), host.stopped())
        .await
        .expect("#1429 the observed assembly stops within the drain budget");
    probe.release();
}

/// #1429: the helper draws the line the acquire path relies on — an unpublished
/// forward reference is `false` (never a silent `true`) and leaves the live ask
/// array untouched, so "there was nothing to sync" cannot be confused with
/// "the publication regressed". `acquiring_a_session_reports_the_skipped_mcp_ask_sync`
/// covers the report the production closure emits for that `false`.
#[test]
fn an_unpublished_mcp_config_reports_the_skipped_sync() {
    let registry: Arc<crate::config::permission::GatewaySessionPermissionRuleSetRegistry> =
        Arc::default();
    let key = JsString::from("session");
    let lease = registry.acquire(&key, None);
    lease.rules().member("ask").update_array(|ask| {
        ask.push(PermissionRule::new("user", "ask", "kept").into());
    });
    let before = ask_tool_names(&lease);
    negative_assert_eq!(
        before,
        vec!["kept".to_owned()],
        "#1429 the fixture starts with one unrelated ask rule"
    );
    negative_assert!(
        !crate::native_process::assembly::sync_acquired_ask_rules(None, &lease.rules(), &key),
        "#1429 an unpublished MCP config reports the skipped sync"
    );
    negative_assert_eq!(
        ask_tool_names(&lease),
        before,
        "#1429 the skipped sync leaves the live ask array untouched"
    );
    lease.release();
}

/// #1429: the miss must be reported from the production prepare closure, not
/// only by calling the helper. The seam suppresses the MCP publication, so the
/// acquire inside a real `submit_turn` meets an unpublished forward reference:
/// it reports the skip (once) and leaves the live ask array alone.
/// #2146: "once" is only observable against a second acquire, so after the
/// first assertions the test rides a second `submit_turn` on a fresh session
/// key and holds the report count at one.
#[tokio::test]
async fn acquiring_a_session_reports_the_skipped_mcp_ask_sync() {
    let scratch = Scratch::new();
    let options = Options::parse(scratch.args()).expect("#1429 options");
    std::fs::write(options.home.join("pilotdeck.yaml"), CONFIG).expect("#1429 config");
    let key = JsString::from("session");
    let registry: Arc<crate::config::permission::GatewaySessionPermissionRuleSetRegistry> =
        Arc::default();
    // A lease of our own keeps the session's rule-set entry alive after the turn.
    let probe = registry.acquire(&key, None);
    probe.rules().member("ask").update_array(|ask| {
        ask.push(PermissionRule::new("user", "ask", "kept").into());
    });
    let reports: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = reports.clone();
    let host = assembly::start_observed_with_permission_rule_sets(
        options.clone(),
        options.environment(Env::new()),
        registry.clone(),
        Arc::new(move |message: &str| {
            sink.lock()
                .expect("#1429 miss reports lock")
                .push(message.to_owned());
        }),
        // Do not publish the MCP config: this is the wiring regression.
        false,
        Arc::new(|_, result| Box::pin(async move { result })),
        Arc::new(|_| {}),
    )
    .await
    .expect("#1429 observed assembly starts");
    let frames = Arc::new(Frames::default());
    let connection = host.connect(frames, "token".into(), "fixture".into());
    connection
        .receive(
            json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"fixture"})
                .to_string(),
        )
        .await
        .expect("#1429 hello");
    let submit = connection.clone();
    let turn = tokio::spawn(async move {
        // The turn itself may fail (the fixture model URL refuses connections);
        // only the session acquire inside it matters here.
        let _ = submit
            .receive(
                json!({"type":"request","id":"turn","method":"submit_turn","params":{"sessionKey":"session","runId":"run","message":"hello","channelKey":"test"}})
                    .to_string(),
            )
            .await;
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while reports.lock().expect("#1429 miss reports lock").is_empty()
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let captured = reports.lock().expect("#1429 miss reports lock").clone();
    negative_assert_eq!(
        captured.len(),
        1,
        "#1429 the acquire path reports the skipped sync exactly once"
    );
    negative_assert!(
        captured[0].contains("MCP ask rule sync skipped"),
        "#1429 the report names the skipped sync"
    );
    negative_assert_eq!(
        ask_tool_names(&probe),
        vec!["kept".to_owned()],
        "#1429 a skipped sync leaves the live ask array untouched"
    );
    // #2146: "exactly once" is only testable against a second acquire — one
    // submit_turn exercises the acquire path once, so deleting the OnceLock
    // dedup left the count assertion above green (the rebase review's
    // boundary finding). The owner serves a known session key from its cached
    // record without re-running the prepare closure, so the second turn rides
    // a fresh key: only that shape guarantees the acquire runs again.
    let submit = connection.clone();
    let second = tokio::spawn(async move {
        // This turn may fail like the first (the fixture model URL refuses
        // connections); only its session acquire matters here.
        let _ = submit
            .receive(
                json!({"type":"request","id":"turn-2","method":"submit_turn","params":{"sessionKey":"session-2","runId":"run-2","message":"hello","channelKey":"test"}})
                    .to_string(),
            )
            .await;
    });
    // The acquire decides while the request settles, so a completed receive
    // proves the second acquire has already reported or stayed silent.
    let settled = tokio::time::timeout(Duration::from_secs(30), second).await;
    negative_assert!(
        settled.is_ok(),
        "#2146 the second submit_turn settles within the deadline"
    );
    let captured = reports.lock().expect("#1429 miss reports lock").clone();
    negative_assert_eq!(
        captured.len(),
        1,
        "#2146 the second acquire reports nothing: one miss report per assembly"
    );
    turn.abort();
    host.request_shutdown(options.drain_budget);
    let _ = tokio::time::timeout(Duration::from_secs(10), host.stopped())
        .await
        .expect("#1429 the observed assembly stops within the drain budget");
    probe.release();
}
