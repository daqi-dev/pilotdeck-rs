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
        host.request_shutdown(Duration::from_millis(250));
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
        .expect("#1071 reserve through original clone");
    negative_assert!(
        cloned.has_transcript_write_reservation(&session),
        "#1071 replacement clone shares reservation"
    );
    let error = cloned
        .reserve_transcript_write(&session, "probe")
        .expect_err("#1071 cloned reservation must conflict");
    negative_assert_eq!(
        error.code,
        Some("SESSION_BUSY"),
        "#1071 replacement clone conflicts on same key"
    );
    cloned.release_transcript_write(&session);
    negative_assert!(
        !original.has_transcript_write_reservation(&session),
        "#1071 replacement clone shares release"
    );
    println!(
        "#1071 clone probe: reserve original -> clone SESSION_BUSY -> release clone -> original unreserved"
    );
}
