use super::*;
use std::{path::PathBuf, sync::Mutex};

struct ResumeHandler {
    mode: String,
    receipt: PathBuf,
    calls: AtomicUsize,
    active_calls: Arc<AtomicUsize>,
    disposed: AtomicUsize,
    calls_at_dispose: AtomicUsize,
    applied: Arc<Mutex<Vec<u64>>>,
    observations: Mutex<Vec<ReconnectObservation>>,
    resumed: Arc<Notify>,
    release_call: Arc<Notify>,
}
impl ResumeHandler {
    fn new(mode: &str, receipt: &Receipt) -> Arc<Self> {
        Arc::new(Self {
            mode: mode.to_owned(),
            receipt: receipt.0.clone(),
            calls: AtomicUsize::new(0),
            active_calls: Arc::new(AtomicUsize::new(0)),
            disposed: AtomicUsize::new(0),
            calls_at_dispose: AtomicUsize::new(usize::MAX),
            applied: Arc::new(Mutex::new(Vec::new())),
            observations: Mutex::new(Vec::new()),
            resumed: Arc::new(Notify::new()),
            release_call: Arc::new(Notify::new()),
        })
    }
    fn assert_observations(&self, cursor: f64, success: bool) {
        let mut expected = vec![ReconnectObservation::Started {
            attempt: 1,
            last_applied_sequence: cursor,
        }];
        if self.mode == "reconnect-instance-changed" {
            expected.push(ReconnectObservation::SidecarInstanceRestarted);
        }
        expected.push(if success {
            ReconnectObservation::Succeeded { attempt: 1 }
        } else {
            ReconnectObservation::Failed { attempt: 1 }
        });
        assert_eq!(
            *self.observations.lock().unwrap(),
            expected,
            "{}",
            self.mode
        );
    }
}
impl HostHandler for ResumeHandler {
    fn dispose(&self) -> HostFuture<()> {
        self.calls_at_dispose
            .store(self.active_calls.load(Ordering::SeqCst), Ordering::SeqCst);
        self.disposed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn module_call(&self, _call: JsValue) -> HostFuture<JsObject> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::fs::write(self.receipt.with_extension("call-started"), "started").unwrap();
        let wait = self.mode == "reconnect-module-pending";
        // The write-failure fixture cuts this response after its first byte;
        // it must exceed pipe capacity so the required write is still pending.
        let large = matches!(
            self.mode.as_str(),
            "reconnect-ack-coalesced" | "reconnect-module-write-failure"
        );
        let budget_cache = self.mode.starts_with("reconnect-budget");
        let release = self.release_call.clone();
        let active = self.active_calls.clone();
        Box::pin(async move {
            active.fetch_add(1, Ordering::SeqCst);
            let _guard = ActiveCall(active);
            if wait {
                release.notified().await;
            }
            let mut response = JsObject::new();
            if large {
                response.insert(
                    "padding".into(),
                    JsValue::String("x".repeat(2 * 1024 * 1024).into()),
                );
            }
            if budget_cache {
                response.insert("padding".into(), JsValue::String("x".repeat(5000).into()));
            }
            Ok(response)
        })
    }
    fn event(&self, event: JsValue) -> HostFuture<()> {
        let sequence = event.get("sequence").and_then(JsValue::as_number).unwrap() as u64;
        let mode = self.mode.clone();
        let applied = self.applied.clone();
        let resumed = self.resumed.clone();
        let release = self.release_call.clone();
        let receipt = self.receipt.clone();
        let active = self.active_calls.clone();
        Box::pin(async move {
            active.fetch_add(1, Ordering::SeqCst);
            let _guard = ActiveCall(active);
            if sequence == 1
                && matches!(
                    mode.as_str(),
                    "reconnect-events" | "reconnect-event-failure"
                )
            {
                std::fs::write(receipt.with_extension("event-started"), "started").unwrap();
                // EOF must be handled while the event is still applying, and
                // its queued successor must survive replacement too.
                resumed.notified().await;
                if mode == "reconnect-event-failure" {
                    return Err(HostError::new(
                        HostErrorKind::Callback,
                        "controlled event failure",
                    ));
                }
            }
            applied.lock().unwrap().push(sequence);
            if sequence == 1 && mode == "reconnect-module-pending" {
                release.notify_one();
            }
            if mode == "reconnect-ack-coalesced" && matches!(sequence, 64 | 65) {
                std::fs::write(
                    receipt.with_extension(format!("applied-{sequence}")),
                    "applied",
                )
                .unwrap();
            }
            Ok(())
        })
    }
    fn observe_transport(&self, observation: ReconnectObservation) {
        self.observations.lock().unwrap().push(observation.clone());
        if matches!(observation, ReconnectObservation::Succeeded { .. }) {
            self.resumed.notify_one();
        }
        if self.mode == "reconnect-observer-panic" {
            panic!("passive observer must not decide execution");
        }
    }
}

fn state(receipt: &Receipt) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(receipt.0.with_extension("state")).unwrap())
        .unwrap()
}
async fn execute(
    mode: &str,
    limits: Limits,
) -> (
    Result<ExecuteResult, HostError>,
    Arc<ResumeHandler>,
    Receipt,
) {
    let (command, receipt) = peer(mode);
    let handler = ResumeHandler::new(mode, &receipt);
    let client = HostClient::spawn_resumable(command, Some(&"fixture".into()), limits)
        .await
        .unwrap();
    let result = client.execute(request(), handler.clone()).await;
    (result, handler, receipt)
}

#[tokio::test]
async fn resume_sends_accepted_cursor_and_replays_from_next_sequence() {
    for (mode, count, cursor) in [
        ("reconnect-accepted", 2, -1.0),
        ("reconnect-terminal", 3, 1.0),
        ("reconnect-observer-panic", 3, 0.0),
    ] {
        let (result, handler, receipt) = execute(mode, limits()).await;
        assert!(
            matches!(result, Ok(ExecuteResult::Terminal(_))),
            "{mode}: {result:?}"
        );
        assert_eq!(
            *handler.applied.lock().unwrap(),
            (0..count).collect::<Vec<_>>(),
            "{mode}"
        );
        handler.assert_observations(cursor, true);
        receipt.assert_pass(mode);
        assert_eq!(state(&receipt)["spawns"], 2);
        assert_eq!(state(&receipt)["resumeCursor"], cursor);
        println!(
            "RESUME_PASS {mode}; applied=0..{count}; cursor={cursor}; observations=started,succeeded; spawns=2"
        );
    }
}

#[tokio::test]
async fn accepted_but_unapplied_events_survive_resume_and_apply_once_in_order() {
    // At EOF event 1 is still applying and 2 is queued. TS client:859, 906
    // sends nextSequence - 1 (=2) as the cursor; the host must still apply
    // 1 and 2 itself, and the peer replays only 3 and the final 4.
    let mode = "reconnect-events";
    let (result, handler, receipt) = execute(mode, limits()).await;
    assert!(
        matches!(result, Ok(ExecuteResult::Terminal(_))),
        "{result:?}"
    );
    assert_eq!(*handler.applied.lock().unwrap(), vec![0, 1, 2, 3, 4]);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
    assert_eq!(handler.calls_at_dispose.load(Ordering::SeqCst), 0);
    handler.assert_observations(2.0, true);
    receipt.assert_pass(mode);
    let state = state(&receipt);
    assert_eq!(state["spawns"], 2);
    assert_eq!(state["resumeCursor"], 2);
    assert_eq!(state["replayed"], serde_json::json!([3, 4]));
    println!(
        "RESUME_QUEUED_PASS cursor=2 (accepted); queued 1,2 applied once after resume; replay=3,4; applied=0..4 in order"
    );
}

#[tokio::test]
async fn first_replayed_event_must_be_exactly_next_sequence_like_ts() {
    // TS client:889-892 has no replay overlap tolerance: next - 1 and next + 1
    // both fail closed with the same message.
    for (mode, got) in [("reconnect-duplicate", 0), ("reconnect-gap", 2)] {
        let (result, handler, receipt) = execute(mode, limits()).await;
        let error = result.unwrap_err();
        assert_eq!(error.kind, HostErrorKind::Protocol, "{mode}: {error}");
        assert!(
            error.message.contains(&format!(
                "Sidecar event sequence mismatch: expected 1, got {got}."
            )),
            "{mode}: {error}"
        );
        assert!(error.accepted);
        assert_eq!(error.last_applied_sequence, Some(0.0));
        assert_eq!(*handler.applied.lock().unwrap(), vec![0], "{mode}");
        handler.assert_observations(0.0, true);
        assert_eq!(state(&receipt)["spawns"], 2);
        println!("REPLAY_STRICT {mode}: {error}; first replayed frame != next fails closed");
    }
}

#[tokio::test]
async fn resume_replays_cached_or_pending_module_response_without_repeating_effect() {
    for mode in [
        "reconnect-module-pending",
        "reconnect-module-cached",
        "reconnect-module-write-failure",
    ] {
        let (result, handler, receipt) = execute(
            mode,
            Limits {
                // Generous bounds: these modes test replay semantics, not timeouts,
                // and a loaded CI host can exceed tighter ones (#230). Under a fully
                // parallel lib run the reconnect path must start Python again, kill
                // and reap the old peer, and rewrite a 2 MiB cached response before
                // the final event; keep this whole-flow watchdog wide enough that a
                // loaded host never decides the result (#560). Test-only: production
                // `Limits::default().execution_timeout` is `None` (no local watchdog, #554).
                close_timeout: Duration::from_millis(500),
                execution_timeout: Some(Duration::from_secs(60)),
                ..limits()
            },
        )
        .await;
        assert!(
            matches!(result, Ok(ExecuteResult::Terminal(_))),
            "{mode}: {result:?}"
        );
        assert_eq!(handler.calls.load(Ordering::SeqCst), 1, "{mode}");
        assert_eq!(handler.disposed.load(Ordering::SeqCst), 1, "{mode}");
        assert_eq!(handler.calls_at_dispose.load(Ordering::SeqCst), 0, "{mode}");
        let count = if mode == "reconnect-module-pending" {
            3
        } else {
            2
        };
        assert_eq!(
            *handler.applied.lock().unwrap(),
            (0..count).collect::<Vec<_>>()
        );
        handler.assert_observations(0.0, true);
        receipt.assert_pass(mode);
        println!("RESUME_PASS {mode}; effect_count=1; cached response/one pending future retained");
    }
}

#[tokio::test]
async fn resume_is_not_attempted_before_acceptance_or_without_capability_or_provider() {
    for mode in [
        "reconnect-before-accepted",
        "reconnect-unsupported-missing",
        "reconnect-unsupported-unary",
        "reconnect-unsupported-disabled",
        "reconnect-unsupported-ack",
        "reconnect-no-provider",
    ] {
        let (mut command, receipt) = peer(mode);
        let handler = ResumeHandler::new(mode, &receipt);
        let client = if mode == "reconnect-no-provider" {
            HostClient::spawn(&mut command, None, limits()).await
        } else {
            HostClient::spawn_resumable(command, None, limits()).await
        }
        .unwrap();
        let error = client
            .execute(request(), handler.clone())
            .await
            .unwrap_err();
        assert_eq!(error.kind, HostErrorKind::Disconnected, "{mode}: {error}");
        let accepted = mode != "reconnect-before-accepted";
        assert_eq!(error.accepted, accepted);
        assert_eq!(error.stream_id, accepted.then(|| "stream".into()));
        assert_eq!(error.last_applied_sequence, accepted.then_some(-1.0));
        assert_eq!(state(&receipt)["spawns"], 1);
        assert!(handler.observations.lock().unwrap().is_empty());
        println!("RESUME_FAIL_CLOSED {mode}: {error}; spawns=1; no reconnect observation");
    }
}

#[tokio::test]
async fn rejected_resume_or_replacement_handshake_observes_failure_exactly_once() {
    for (mode, expected) in [
        ("reconnect-rejected", "CURSOR_EXPIRED"),
        ("reconnect-wrong-reply", "active resume request"),
        ("reconnect-wrong-stream", "changed the stream identity"),
        ("reconnect-event-before-resume", "non-response"),
        ("reconnect-call-before-resume", "non-response"),
        ("reconnect-malformed", "Malformed NDJSON"),
        ("reconnect-instance-changed", "instance restarted"),
        ("reconnect-module-changed", "changed its module identity"),
        ("reconnect-handshake-eof", "stdout closed"),
        ("reconnect-handshake-timeout", "resume handshake timed out"),
    ] {
        let (result, handler, receipt) = execute(
            mode,
            Limits {
                // The timeout mode never answers; the others must not lose to a
                // loaded host's scheduling (#230).
                // Must stay well below execution_timeout, or that fires first.
                handshake_timeout: Duration::from_secs(1),
                close_timeout: Duration::from_millis(500),
                execution_timeout: Some(Duration::from_secs(10)),
                ..limits()
            },
        )
        .await;
        let error = result.unwrap_err();
        assert!(error.message.contains(expected), "{mode}: {error}");
        assert!(error.accepted);
        assert_eq!(error.stream_id, Some("stream".into()));
        assert_eq!(error.last_applied_sequence, Some(0.0));
        assert_eq!(state(&receipt)["spawns"], 2);
        handler.assert_observations(0.0, false);
        println!("RESUME_REJECT {mode}: {error}; observations=started,failed (once)");
    }
}

#[tokio::test]
async fn reconnect_retains_callback_cache_in_the_same_byte_budget() {
    for mode in ["reconnect-budget", "reconnect-budget-control"] {
        let (result, handler, receipt) = execute(
            mode,
            Limits {
                max_frame_bytes: 8000,
                max_buffered_bytes: 18000,
                ..limits()
            },
        )
        .await;
        if mode == "reconnect-budget" {
            let error = result.unwrap_err();
            assert_eq!(error.kind, HostErrorKind::Limit, "{error}");
            assert!(error.message.contains("wire-byte budget"), "{error}");
            assert_eq!(error.last_applied_sequence, Some(0.0));
            assert_eq!(*handler.applied.lock().unwrap(), vec![0]);
        } else {
            assert!(
                matches!(result, Ok(ExecuteResult::Terminal(_))),
                "{result:?}"
            );
            assert_eq!(*handler.applied.lock().unwrap(), vec![0, 1, 2]);
        }
        handler.assert_observations(0.0, true);
        assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
        receipt.assert_pass(mode);
        println!(
            "RECONNECT_BUDGET_PASS {mode}; cached 5000 bytes count across connections; maximum=18000"
        );
    }
}

#[tokio::test]
async fn reconnect_preserves_command_environment_clear_removals_and_cwd() {
    let mode = "reconnect-command";
    let (mut command, receipt) = peer(mode);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOST_REPLAY_ENV", "retained")
        .env("HOST_REPLAY_REMOVE", "must-not-survive")
        .env_remove("HOST_REPLAY_REMOVE")
        .current_dir(receipt.0.parent().unwrap());
    let handler = ResumeHandler::new(mode, &receipt);
    let client = HostClient::spawn_resumable(
        command,
        None,
        Limits {
            // Two independent clocks, both test-only. `handshake_timeout` bounds
            // the initial hello/capabilities exchange (mod.rs:422-424) and the
            // post-reconnect handshake (recovery.rs:55); the recorded load
            // failures for this test are exactly that bound expiring on Python
            // startup (s4.log:1159, s7.log:1159), so it must be widened too.
            // `execution_timeout` only starts at execute (execution.rs:134) and
            // bounds the rest of the flow. Production keeps its own defaults.
            handshake_timeout: Duration::from_secs(20),
            execution_timeout: Some(Duration::from_secs(20)),
            ..limits()
        },
    )
    .await
    .unwrap();
    let result = client.execute(request(), handler.clone()).await;
    assert!(
        matches!(result, Ok(ExecuteResult::Terminal(_))),
        "{result:?}"
    );
    handler.assert_observations(0.0, true);
    assert_eq!(state(&receipt)["spawns"], 2);
    receipt.assert_pass(mode);
    println!("RECONNECT_COMMAND_PASS both spawns retain env_clear, env_remove, cwd");
}

#[tokio::test]
async fn successful_resume_never_observes_failure_or_retries_after_bad_replay() {
    for (mode, kind, expected) in [
        (
            "reconnect-backward",
            HostErrorKind::Protocol,
            "Sidecar event sequence mismatch: expected 3, got 1.",
        ),
        (
            "reconnect-module-conflict",
            HostErrorKind::Protocol,
            "conflicting immutable identity",
        ),
        (
            "reconnect-event-failure",
            HostErrorKind::Callback,
            "controlled event failure",
        ),
        (
            "reconnect-twice",
            HostErrorKind::Disconnected,
            "stdout closed",
        ),
    ] {
        let (result, handler, receipt) = execute(mode, limits()).await;
        let error = result.unwrap_err();
        assert_eq!(error.kind, kind, "{mode}: {error}");
        assert!(error.message.contains(expected), "{mode}: {error}");
        assert!(error.accepted);
        assert_eq!(state(&receipt)["spawns"], 2);
        handler.assert_observations(
            match mode {
                "reconnect-backward" => 1.0,
                // Event 1 is applying and 2 queued at EOF: both accepted.
                "reconnect-event-failure" => 2.0,
                _ => 0.0,
            },
            true,
        );
        assert!(handler.calls.load(Ordering::SeqCst) <= 1);
        println!("REPLAY_REJECT {mode}: {error}; observations=started,succeeded; no third spawn");
    }
}

#[tokio::test]
async fn coalesced_ack_cumulative_cleanup_slow_pipe_disconnect_and_resume_are_lossless() {
    let mode = "reconnect-ack-coalesced";
    let (result, handler, receipt) = execute(
        mode,
        Limits {
            // Same reconnect + 2 MiB cached-response path as #560 above; keep the
            // whole-flow watchdog wide enough that a loaded host never decides the
            // result. Test-only.
            execution_timeout: Some(Duration::from_secs(60)),
            ..limits()
        },
    )
    .await;
    assert!(
        matches!(result, Ok(ExecuteResult::Terminal(_))),
        "{result:?}"
    );
    assert_eq!(
        *handler.applied.lock().unwrap(),
        (0..68).collect::<Vec<_>>()
    );
    assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
    handler.assert_observations(65.0, true);
    receipt.assert_pass(mode);
    let state = state(&receipt);
    assert_eq!(state["spawns"], 2);
    assert_eq!(state["acks"], serde_json::json!([0, 64, 66]));
    assert_eq!(state["resumeCursor"], 65);
    // Cursor = accepted (TS client:859), so replay starts at next=66, no overlap.
    assert_eq!(state["replayed"], serde_json::json!([66, 67]));
    assert_eq!(state["retained"], serde_json::json!([67]));
    println!(
        "ACK_REPLAY_PASS slow 2 MiB write; ACK 1..64 coalesced to 64; cumulative cleanup 0..64; ACK 65 lost; resume cursor=65 (accepted); replay=66,67 (no duplicate); applied=0..67 exactly once; effect_count=1"
    );
}
