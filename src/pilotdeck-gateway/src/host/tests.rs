use super::*;
use pilotdeck_protocol::parse_json;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::{sync::Notify, time::Instant};

struct Handler {
    calls: AtomicUsize,
    events: AtomicUsize,
    applied_events: Arc<AtomicUsize>,
    turn_completed_events: AtomicUsize,
    release: Arc<Notify>,
    wait_for_event: bool,
    never_complete: bool,
    slow_event: bool,
    active_calls: Arc<AtomicUsize>,
    cancellation: Option<Cancellation>,
    cancel_reason: JsString,
    cancel_from_module: bool,
    event_never: bool,
    large_response: bool,
}
impl Handler {
    fn new(mode: &str) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            active_calls: Arc::new(AtomicUsize::new(0)),
            events: AtomicUsize::new(0),
            applied_events: Arc::new(AtomicUsize::new(0)),
            turn_completed_events: AtomicUsize::new(0),
            release: Arc::new(Notify::new()),
            wait_for_event: mode == "callback-progress",
            never_complete: matches!(
                mode,
                "callback-disconnect"
                    | "callback-timeout"
                    | "duplicate-conflict"
                    | "terminal-pending-callback"
                    | "module-call-limit"
                    | "active-call-limit"
                    | "duplicate-call-limit"
                    | "grandchild-timeout"
                    | "grandchild-drop"
            ),
            slow_event: mode == "slow-event",
            cancellation: None,
            cancel_reason: if mode.starts_with("cancel-compaction") {
                "compaction_persistence_failed".into()
            } else if mode == "cancel-empty-reason" {
                "".into()
            } else {
                "test_abort".into()
            },
            cancel_from_module: mode.starts_with("cancel-before-"),
            event_never: matches!(mode, "event-queue-limit" | "event-byte-limit"),
            large_response: mode == "response-byte-limit",
        })
    }
}
impl HostHandler for Handler {
    fn module_call(&self, _call: JsValue) -> HostFuture<JsObject> {
        if self.cancel_from_module
            && let Some(cancel) = &self.cancellation
        {
            cancel.cancel(Some(self.cancel_reason.clone()));
            cancel.cancel(Some("ignored-second-reason".into()));
        }
        let large_response = self.large_response;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let release = self.release.clone();
        let (wait, never) = (self.wait_for_event, self.never_complete);
        let active = self.active_calls.clone();
        Box::pin(async move {
            active.fetch_add(1, Ordering::SeqCst);
            let _guard = ActiveCall(active);
            if never {
                std::future::pending::<()>().await;
            }
            if wait {
                release.notified().await;
            }
            let mut response = JsObject::new();
            if large_response {
                response.insert("padding".into(), JsValue::String("x".repeat(8192).into()));
            }
            Ok(response)
        })
    }
    fn event(&self, event: JsValue) -> HostFuture<()> {
        if !self.cancel_from_module
            && let Some(cancel) = &self.cancellation
        {
            cancel.cancel(Some(self.cancel_reason.clone()));
            cancel.cancel(Some("ignored-second-reason".into()));
        }
        let never = self.event_never;
        let applied = self.applied_events.clone();
        self.events.fetch_add(1, Ordering::SeqCst);
        if text_is(
            event.get("payload").and_then(|payload| payload.get("type")),
            "turn_completed",
        ) {
            self.turn_completed_events.fetch_add(1, Ordering::SeqCst);
        }
        self.release.notify_one();
        let slow = self.slow_event;
        Box::pin(async move {
            if never {
                std::future::pending::<()>().await;
            }
            if slow {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            applied.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
struct ActiveCall(Arc<AtomicUsize>);
impl Drop for ActiveCall {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn limits() -> Limits {
    Limits {
        handshake_timeout: Duration::from_secs(2),
        execution_timeout: Duration::from_secs(2),
        close_timeout: Duration::from_secs(1),
        ..Limits::default()
    }
}
struct Receipt(std::path::PathBuf);
impl Receipt {
    fn assert_pass(&self, mode: &str) {
        assert_eq!(
            std::fs::read_to_string(&self.0).unwrap(),
            format!("PASS {mode}\n"),
            "peer protocol assertions did not finish"
        );
    }
}
impl Drop for Receipt {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}
fn peer(mode: &str) -> (Command, Receipt) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let owned = std::env::temp_dir().join(format!(
        "pilotdeck-host-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    // Exclusive directory: PID reuse can never turn an old PASS into evidence.
    std::fs::create_dir(&owned).unwrap();
    let receipt = Receipt(owned.join(format!("{mode}.txt")));
    let mut command = command(mode);
    command.arg(&receipt.0);
    (command, receipt)
}
fn command(mode: &str) -> Command {
    let mut command = Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/a6-host/fixture.py"
        ))
        .arg(mode);
    command
}
fn request() -> ExecuteRequest {
    ExecuteRequest {
        identity: ExecutionIdentity {
            run_id: "run".into(),
            operation_id: "operation".into(),
            request_id: "request".into(),
            session_id: "session".into(),
            turn_id: "turn".into(),
        },
        payload: JsObject::new(),
        idempotency_key: None,
        operation_deadline: None,
    }
}
async fn run(mode: &str) -> Result<ExecuteResult, HostError> {
    let (mut command, receipt) = peer(mode);
    let client = HostClient::spawn(&mut command, Some(&"fixture".into()), limits()).await?;
    let result = client.execute(request(), Handler::new(mode)).await;
    if result.is_ok() {
        receipt.assert_pass(mode);
    }
    result
}

#[tokio::test]
async fn handshake_execute_ack_and_terminal() {
    for mode in [
        "positive",
        "slow-event",
        "no-ack",
        "ack-disabled",
        "resume-support-missing",
        "resume-support-unary",
        "duplicate-acceptance",
        "duplicate-acceptance-new-stream",
        "stderr-flood",
        "rejected",
        "cleanup-hang",
    ] {
        let started = Instant::now();
        let result = run(mode).await.unwrap_or_else(|e| panic!("{mode}: {e}"));
        assert_eq!(
            matches!(result, ExecuteResult::Rejected(_)),
            mode == "rejected"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "unbounded cleanup: {mode}"
        );
        println!("HOST_PASS {mode}");
    }
}
#[tokio::test]
async fn terminal_survives_peer_exit_without_reading_ack() {
    let mode = "terminal-then-exit";
    let handler = Handler::new(mode);
    let (mut command, receipt) = peer(mode);
    let client = HostClient::spawn(&mut command, None, limits())
        .await
        .unwrap();
    let result = client.execute(request(), handler.clone()).await.unwrap();
    assert!(matches!(result, ExecuteResult::Terminal(_)));
    receipt.assert_pass(mode);
    assert_eq!(handler.events.load(Ordering::SeqCst), 2);
    println!("HOST_PASS {mode}; applied=2; peer closed stdin before events");
}
#[tokio::test]
async fn ack_failures_and_unknown_replies_follow_ts_ignore_behavior() {
    for mode in ["ack-failure", "unknown-response"] {
        let handler = Handler::new(mode);
        let (mut command, receipt) = peer(mode);
        let client = HostClient::spawn(&mut command, None, limits())
            .await
            .unwrap();
        let result = client.execute(request(), handler.clone()).await.unwrap();
        assert!(matches!(result, ExecuteResult::Terminal(_)), "{mode}");
        receipt.assert_pass(mode);
        assert_eq!(handler.events.load(Ordering::SeqCst), 2, "{mode}");
        println!("HOST_PASS {mode}");
    }
}
#[tokio::test]
async fn cancelled_and_failed_terminal_results_are_accepted() {
    for (mode, outcome, result_type) in [
        ("cancelled", "cancelled", "aborted"),
        ("failed-error", "failed", "error"),
        ("failed-max-turns", "failed", "max_turns"),
    ] {
        let ExecuteResult::Terminal(event) = run(mode).await.unwrap() else {
            panic!("expected terminal: {mode}");
        };
        assert!(text_is(event.get("outcome"), outcome), "{mode}");
        assert!(
            text_is(
                event
                    .get("payload")
                    .unwrap()
                    .get("result")
                    .unwrap()
                    .get("type"),
                result_type
            ),
            "{mode}"
        );
        println!("HOST_PASS {mode}");
    }
}
#[tokio::test]
async fn turn_completed_projection_is_neither_applied_nor_acked() {
    let mode = "turn-completed";
    let handler = Handler::new(mode);
    let (mut command, receipt) = peer(mode);
    let client = HostClient::spawn(&mut command, None, limits())
        .await
        .unwrap();
    let result = client.execute(request(), handler.clone()).await.unwrap();
    assert!(matches!(result, ExecuteResult::Terminal(_)));
    receipt.assert_pass(mode);
    // Two text_delta events plus the authoritative final; sequence 1 is suppressed.
    assert_eq!(handler.events.load(Ordering::SeqCst), 3);
    assert_eq!(handler.turn_completed_events.load(Ordering::SeqCst), 0);
    println!("HOST_PASS {mode}; applied=3; peer checked ACKs 0,2 only");
}
#[tokio::test]
async fn four_required_negative_controls() {
    for (mode, kind, accepted, reason) in [
        ("identity", HostErrorKind::Protocol, true, "event identity"),
        (
            "duplicate-conflict",
            HostErrorKind::Protocol,
            true,
            "conflicting immutable identity",
        ),
        (
            "stale-generation",
            HostErrorKind::Protocol,
            false,
            "binding during handshake",
        ),
        (
            "disconnect",
            HostErrorKind::Disconnected,
            true,
            "before execute reached a terminal",
        ),
    ] {
        let error = run(mode).await.expect_err(mode);
        assert_eq!(error.kind, kind, "{mode}: {error}");
        assert_eq!(error.accepted, accepted, "{mode}: {error}");
        assert!(error.message.contains(reason), "{mode}: {error}");
        println!("HOST_REJECT {mode}: {error}");
    }
}
#[tokio::test]
async fn handshake_and_execute_correlation_negative_controls() {
    for (mode, reason) in [
        (
            "execute-request-mismatch",
            "execute response does not match the active request",
        ),
        ("hello-rejected", "hello response rejected"),
        ("capabilities-rejected", "capabilities response rejected"),
        ("empty-stream", "streamId"),
        ("accepted-final", "cannot be final"),
        ("rejected-nonfinal", "CONTROLLED_REJECTION"),
        ("rejected-final-cancelled", "rejected execute request"),
        ("rejected-error-message", "controlled failure message"),
        ("rejected-unknown", "unknown error"),
    ] {
        let error = run(mode).await.expect_err(mode);
        assert_eq!(error.kind, HostErrorKind::Protocol, "{mode}: {error}");
        assert!(!error.accepted, "{mode}: {error}");
        assert!(error.message.contains(reason), "{mode}: {error}");
        println!("HOST_REJECT {mode}: {error}");
    }
    let error = HostClient::spawn(
        &mut command("positive"),
        Some(&"other-module".into()),
        limits(),
    )
    .await
    .err()
    .expect("expected module identity rejection");
    assert_eq!(error.kind, HostErrorKind::Protocol);
    assert!(!error.accepted);
    assert!(
        error
            .message
            .contains("module identity does not match configured implementation"),
        "{error}"
    );
    println!("HOST_REJECT expected-module-mismatch: {error}");
}
#[tokio::test]
async fn unknown_fixture_mode_fails_immediately() {
    // Keep stdin open: validation must happen before the peer attempts to read hello.
    let mut child = command("misspelled-mode")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let _stdin = child.stdin.take().unwrap();
    let output = timeout(Duration::from_secs(2), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown fixture mode: misspelled-mode")
    );
}
#[tokio::test]
async fn oversized_limits_are_rejected_before_spawn() {
    const { assert!((1 << 20) <= tokio::sync::Semaphore::MAX_PERMITS) };
    for field in [
        "queue_capacity",
        "max_module_calls",
        "max_frame_bytes",
        "max_stderr_bytes",
        "max_buffered_bytes",
    ] {
        let maximum = if field == "max_buffered_bytes" {
            1 << 30
        } else if matches!(field, "queue_capacity" | "max_module_calls") {
            1 << 20
        } else {
            64 << 20
        };
        for value in [maximum, maximum + 1, usize::MAX] {
            let mut bounded = limits();
            match field {
                "queue_capacity" => bounded.queue_capacity = value,
                "max_module_calls" => bounded.max_module_calls = value,
                "max_frame_bytes" => bounded.max_frame_bytes = value,
                "max_stderr_bytes" => bounded.max_stderr_bytes = value,
                "max_buffered_bytes" => bounded.max_buffered_bytes = value,
                _ => unreachable!(),
            }
            if value == maximum {
                bounded.validate().unwrap();
                continue;
            }
            let error = HostClient::spawn(
                &mut Command::new("nonexistent-sidecar-for-limit-test"),
                None,
                bounded,
            )
            .await
            .err()
            .expect("oversized limit must fail before spawning");
            assert_eq!(error.kind, HostErrorKind::Limit, "{field}: {error}");
            assert!(
                error
                    .message
                    .contains(&format!("{field} exceeds maximum {maximum}")),
                "{error}"
            );
            println!("HOST_REJECT {field}={value}: {error}");
        }
    }
}
#[tokio::test]
async fn invalid_frames_sequences_and_terminal_never_succeed() {
    for (mode, reason) in [
        ("sequence", "sequence mismatch"),
        ("before-accepted", "before acceptance"),
        ("bad-session", "active session"),
        ("stream-mismatch", "event identity"),
        ("missing-messages", "missing messages"),
        ("wrong-call-conflict", "conflicting immutable identity"),
        ("wrong-call-replay", "conflicting immutable identity"),
        (
            "event-generation",
            "Connection binding belongs to transport context",
        ),
        ("bad-terminal", "outcome conflicts"),
        ("cancelled-success", "outcome conflicts"),
        ("unknown", "reconciliation is required"),
        ("no-streaming", "streaming execute"),
        ("malformed", "Malformed NDJSON"),
    ] {
        let error = run(mode).await.expect_err(mode);
        assert!(error.message.contains(reason), "{mode}: {error}");
        println!("HOST_REJECT {mode}: {error}");
    }
}
#[tokio::test]
async fn callbacks_do_not_block_input_or_run_twice() {
    for mode in ["duplicate-same", "callback-progress", "wrong-call"] {
        let handler = Handler::new(mode);
        let (mut command, receipt) = peer(mode);
        let client = HostClient::spawn(&mut command, None, limits())
            .await
            .unwrap();
        client
            .execute(request(), handler.clone())
            .await
            .unwrap_or_else(|e| panic!("{mode}: {e}"));
        receipt.assert_pass(mode);
        assert_eq!(
            handler.calls.load(Ordering::SeqCst),
            usize::from(mode != "wrong-call")
        );
        println!("HOST_PASS {mode}");
    }
    let started = Instant::now();
    let error = run("callback-disconnect").await.unwrap_err();
    assert_eq!(error.kind, HostErrorKind::Disconnected);
    assert!(error.accepted && started.elapsed() < Duration::from_secs(1));
    println!("HOST_REJECT callback-disconnect: {error}");
}
#[tokio::test]
async fn timeout_and_frame_limits_close_owned_processes() {
    let mut bounded = limits();
    bounded.handshake_timeout = Duration::from_millis(100);
    let error = HostClient::spawn(&mut command("handshake-hang"), None, bounded)
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, HostErrorKind::Timeout);
    bounded.max_frame_bytes = 1024;
    let error = HostClient::spawn(&mut command("oversized"), None, bounded)
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, HostErrorKind::Limit);
    bounded.execution_timeout = Duration::from_millis(100);
    bounded.handshake_timeout = Duration::from_secs(2);
    let client = HostClient::spawn(&mut command("callback-timeout"), None, bounded)
        .await
        .unwrap();
    let error = client
        .execute(request(), Handler::new("callback-timeout"))
        .await
        .unwrap_err();
    assert_eq!(error.kind, HostErrorKind::Timeout);
    assert!(error.accepted);
    println!("HOST_REJECT callback-timeout: {error}");
}
#[test]
fn duplicate_payload_comparison_preserves_js_wire_facts() {
    let parse = |payload: &str| {
        parse_json(&format!(r#"{{"messageId":"x","runId":"r","operationId":"o","requestId":"q","module":"model","payload":{payload}}}"#)).unwrap()
    };
    let first = parse(r#"{"a":1,"b":"\ud800"}"#);
    assert!(execution::same_call(&first, &first).unwrap());
    assert!(!execution::same_call(&first, &parse(r#"{"b":"\ud800","a":1}"#)).unwrap());
    assert!(!execution::same_call(&first, &parse(r#"{"a":1,"b":"�"}"#)).unwrap());
}

async fn assert_reaped(pid: u32) {
    timeout(Duration::from_secs(1), async {
        loop {
            match rustix::process::test_kill_process(
                rustix::process::Pid::from_raw(pid as i32).unwrap(),
            ) {
                Err(rustix::io::Errno::SRCH) => break,
                Ok(()) => tokio::time::sleep(Duration::from_millis(5)).await,
                Err(error) => panic!("cannot inspect owned child {pid}: {error}"),
            }
        }
    })
    .await
    .expect("owned sidecar was not reaped");
}
#[tokio::test]
async fn dropped_execute_reaps_child_and_aborts_callback() {
    let client = HostClient::spawn(&mut command("callback-timeout"), None, limits())
        .await
        .unwrap();
    let pid = client.child_id().unwrap();
    let handler = Handler::new("callback-timeout");
    let task = tokio::spawn(client.execute(request(), handler.clone()));
    timeout(Duration::from_secs(1), async {
        while handler.active_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_reaped(pid).await;
    timeout(Duration::from_secs(1), async {
        while handler.active_calls.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("callback future was not dropped");
    println!("HOST_PASS dropped execute reaped pid={pid}; callback Drop observed");
}
#[tokio::test]
async fn explicit_close_reaps_child() {
    let client = HostClient::spawn(&mut command("positive"), None, limits())
        .await
        .unwrap();
    let pid = client.child_id().unwrap();
    client.close().await.unwrap();
    assert_reaped(pid).await;
    println!("HOST_PASS close reaped pid={pid}");
}
#[tokio::test]
async fn close_timeout_kills_and_reaps_unresponsive_child_without_receipt() {
    let mut bounded = limits();
    bounded.close_timeout = Duration::from_millis(50);
    let client = HostClient::spawn(&mut command("close-hang"), None, bounded)
        .await
        .unwrap();
    let pid = client.child_id().unwrap();
    timeout(Duration::from_secs(1), client.close())
        .await
        .unwrap()
        .unwrap();
    assert_reaped(pid).await;
    println!("HOST_PASS close timeout killed and reaped pid={pid}");
}

async fn cancel_run(mode: &str) -> (Result<ExecuteResult, HostError>, Arc<Handler>) {
    let cancellation = Cancellation::default();
    let mut handler = Handler::new(mode);
    if mode != "cancel-compaction-no-local" {
        Arc::get_mut(&mut handler).unwrap().cancellation = Some(cancellation.clone());
    }
    let (mut command, receipt) = peer(mode);
    let bounded = Limits {
        execution_timeout: Duration::from_millis(400),
        close_timeout: Duration::from_millis(100),
        ..limits()
    };
    let client = HostClient::spawn(&mut command, None, bounded)
        .await
        .unwrap();
    let result = client
        .execute_cancellable(request(), handler.clone(), cancellation)
        .await;
    if result.is_ok() {
        receipt.assert_pass(mode);
    }
    (result, handler)
}

#[tokio::test]
async fn pre_cancelled_input_does_not_send_execute() {
    let (mut command, receipt) = peer("cancel-pre-execute");
    let client = HostClient::spawn(&mut command, None, limits())
        .await
        .unwrap();
    let cancellation = Cancellation::default();
    cancellation.cancel(None);
    let handler = Handler::new("cancel-pre-execute");
    let result = client
        .execute_cancellable(request(), handler.clone(), cancellation)
        .await
        .unwrap();
    assert!(matches!(result, ExecuteResult::Aborted { terminal: None }));
    assert_eq!(handler.events.load(Ordering::SeqCst), 0);
    receipt.assert_pass("cancel-pre-execute");
}

#[tokio::test]
async fn cancel_waits_for_acceptance_and_authoritative_terminal_once() {
    for mode in [
        "cancel-before-accepted",
        "cancel-after-accepted",
        "cancel-nack",
        "cancel-execute-capability",
        "cancel-empty-reason",
        "cancel-success",
        "cancel-unsupported",
        "cancel-disabled",
        "cancel-write-failure",
        "cancel-duplicate-acceptance",
    ] {
        let (result, _) = cancel_run(mode).await;
        let ExecuteResult::Terminal(event) = result.unwrap_or_else(|e| panic!("{mode}: {e}"))
        else {
            panic!("{mode}")
        };
        let expected = if matches!(
            mode,
            "cancel-success" | "cancel-unsupported" | "cancel-disabled" | "cancel-write-failure"
        ) {
            "completed"
        } else {
            "cancelled"
        };
        assert!(text_is(event.get("outcome"), expected), "{mode}");
        println!("HOST_CANCEL_PASS {mode}: {expected}");
    }
}

#[tokio::test]
async fn compaction_abort_fallback_matches_ts_shape_and_three_conditions() {
    for mode in [
        "cancel-compaction",
        "cancel-compaction-no-messages",
        "cancel-compaction-malformed",
    ] {
        let (result, handler) = cancel_run(mode).await;
        assert!(
            matches!(
                result.unwrap(),
                ExecuteResult::Aborted { terminal: Some(_) }
            ),
            "{mode}"
        );
        assert_eq!(handler.events.load(Ordering::SeqCst), 2);
    }
    for mode in [
        "cancel-conflict",
        "cancel-compaction-conflict",
        "cancel-compaction-no-local",
        "cancel-missing-result",
        "cancel-compaction-completed",
    ] {
        let (result, _) = cancel_run(mode).await;
        let error = result.unwrap_err();
        assert_eq!(error.kind, HostErrorKind::Protocol, "{mode}: {error}");
        assert!(error.accepted, "{mode}");
    }
}

#[tokio::test]
async fn cancel_disconnect_and_timeout_preserve_unknown_progress() {
    for (mode, kind) in [
        ("cancel-disconnect", HostErrorKind::Disconnected),
        ("cancel-timeout", HostErrorKind::Timeout),
    ] {
        let (result, _) = cancel_run(mode).await;
        let error = result.unwrap_err();
        assert_eq!(error.kind, kind, "{error}");
        assert!(error.accepted);
        assert_eq!(error.stream_id, Some("stream".into()));
        assert_eq!(error.last_applied_sequence, Some(0.0));
        if mode == "cancel-disconnect" {
            assert!(error.message.contains("CANCEL-DISCONNECT-TAIL"));
        }
    }
    for (mode, kind) in [
        ("cancel-before-timeout", HostErrorKind::Timeout),
        ("cancel-before-disconnect", HostErrorKind::Disconnected),
    ] {
        let (result, _) = cancel_run(mode).await;
        let error = result.unwrap_err();
        assert_eq!(error.kind, kind, "{error}");
        assert!(!error.accepted);
        assert_eq!(error.stream_id, None);
        assert_eq!(error.last_applied_sequence, None);
        assert!(!error.message.contains("AssertionError"), "{error}");
    }
    // Race EOF with successful event application; compare observed application,
    // independent of which ready branch the parent's select chooses first.
    for _ in 0..8 {
        let mode = "event-then-disconnect";
        let client = HostClient::spawn(&mut command(mode), None, limits())
            .await
            .unwrap();
        let handler = Handler::new(mode);
        let error = client
            .execute(request(), handler.clone())
            .await
            .unwrap_err();
        assert_eq!(error.kind, HostErrorKind::Disconnected);
        assert_eq!(
            error.last_applied_sequence,
            Some(handler.applied_events.load(Ordering::SeqCst) as f64 - 1.0)
        );
    }
    let error = run("disconnect").await.unwrap_err();
    assert_eq!(error.last_applied_sequence, Some(-1.0));
    assert_eq!(error.stream_id, Some("stream".into()));
    let error = run("rejected-nonfinal").await.unwrap_err();
    assert_eq!(error.last_applied_sequence, None);
    assert_eq!(error.stream_id, None);
}

#[tokio::test]
async fn applied_terminal_bounds_pending_callback_drain() {
    let mode = "terminal-pending-callback";
    let (mut command, receipt) = peer(mode);
    let bounded = Limits {
        terminal_drain_timeout: Duration::from_millis(300),
        execution_timeout: Duration::from_millis(200),
        ..limits()
    };
    let client = HostClient::spawn(&mut command, None, bounded)
        .await
        .unwrap();
    let handler = Handler::new(mode);
    let started = Instant::now();
    let result = client.execute(request(), handler.clone()).await.unwrap();
    assert!(matches!(result, ExecuteResult::Terminal(_)));
    assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
    assert_eq!(handler.active_calls.load(Ordering::SeqCst), 0);
    assert_eq!(handler.events.load(Ordering::SeqCst), 1);
    assert!(started.elapsed() >= bounded.terminal_drain_timeout);
    assert!(started.elapsed() < Duration::from_secs(1));
    receipt.assert_pass(mode);
}

#[tokio::test]
async fn close_drains_stderr_exit_tail_before_aborting_pump() {
    let bounded = Limits {
        max_stderr_bytes: 64,
        ..limits()
    };
    let error = HostClient::spawn(&mut command("stderr-on-close"), None, bounded)
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, HostErrorKind::Protocol);
    assert!(error.message.contains("EXIT-STDERR-TAIL"), "{error}");
    assert!(!error.message.contains("discarded-prefix"));
    assert!(error.message.split("stderr: ").nth(1).unwrap().len() <= 64);
}

#[tokio::test]
async fn every_execution_limit_fails_closed_and_drops_callbacks() {
    for (mode, reason) in [
        ("module-call-limit", "module callback limit"),
        ("active-call-limit", "module callback limit"),
        ("wrong-call-limit", "module callback limit"),
        ("duplicate-call-limit", "pending duplicate"),
        ("event-queue-limit", "event application queue full"),
        ("cache-byte-limit", "wire-byte budget"),
        ("response-byte-limit", "wire-byte budget"),
        ("event-byte-limit", "wire-byte budget"),
    ] {
        let mut bounded = limits();
        match mode {
            "module-call-limit" | "wrong-call-limit" => bounded.max_module_calls = 2,
            "active-call-limit" | "duplicate-call-limit" | "event-queue-limit" => {
                bounded.queue_capacity = 2
            }
            _ => bounded.max_buffered_bytes = 3000,
        }
        let client = HostClient::spawn(&mut command(mode), None, bounded)
            .await
            .unwrap();
        let handler = Handler::new(mode);
        let error = client
            .execute(request(), handler.clone())
            .await
            .unwrap_err();
        assert_eq!(error.kind, HostErrorKind::Limit, "{mode}: {error}");
        assert!(error.accepted, "{mode}: {error}");
        assert!(error.message.contains(reason), "{mode}: {error}");
        timeout(Duration::from_secs(1), async {
            while handler.active_calls.load(Ordering::SeqCst) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        println!("HOST_LIMIT_PASS {mode}: {error}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn close_timeout_execute_timeout_drop_and_exited_leader_kill_grandchildren() {
    for mode in [
        "grandchild-close",
        "grandchild-timeout",
        "grandchild-drop",
        "grandchild-exit",
    ] {
        let (mut command, receipt) = peer(mode);
        let bounded = Limits {
            close_timeout: Duration::from_millis(40),
            execution_timeout: Duration::from_millis(100),
            ..limits()
        };
        let client = HostClient::spawn(&mut command, None, bounded)
            .await
            .unwrap();
        let pid: u32 = std::fs::read_to_string(receipt.0.with_extension("pid"))
            .unwrap()
            .parse()
            .unwrap();
        if mode == "grandchild-timeout" {
            let error = client
                .execute(request(), Handler::new(mode))
                .await
                .unwrap_err();
            assert_eq!(error.kind, HostErrorKind::Timeout);
        } else if mode == "grandchild-drop" {
            drop(client);
        } else {
            client.close().await.unwrap();
        }
        assert_reaped(pid).await;
        println!("HOST_GROUP_PASS {mode}: grandchild={pid}");
    }
}
