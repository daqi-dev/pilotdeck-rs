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
        // Never change the test process's shared cwd. For the cwd=project
        // rows, register that existing directory under our isolated home.
        let project = if row["cwd"] == "project" {
            std::env::current_dir().expect("#1137 current marker project")
        } else {
            scratch.0.join("project")
        };
        let other = scratch.0.join("other");
        let options = Options {
            home: scratch.0.join("home"),
            project,
            listen: "127.0.0.1:0".parse().expect("#1137 marker listen address"),
            token_file: scratch.0.join("unused-token"),
            sidecar: scratch.0.join("unused-sidecar"),
            sidecar_args: vec![],
            drain_budget: Duration::from_secs(1),
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
        let result = assembly::register_project(&options);
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
