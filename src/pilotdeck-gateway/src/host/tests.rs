use super::*;
use pilotdeck_protocol::parse_json;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::{sync::Notify, time::Instant};

mod binding;
mod cancel_exit;
mod event;
mod model;
mod modules;
mod resume;

/// Test-only observations plus an opt-in reconnect gate. Production builds
/// contain neither; the gate separates real child exit from virtual timers.
#[derive(Clone, Default)]
pub(super) struct ExecutionSignals {
    pub(super) loop_exited: Arc<std::sync::atomic::AtomicBool>,
    pub(super) final_received: Arc<std::sync::atomic::AtomicBool>,
    pub(super) last_applied_bits: Arc<AtomicU64>,
    pub(super) cancel_deadline: tokio::sync::watch::Sender<Option<Instant>>,
    pub(super) reconnect_started: tokio::sync::watch::Sender<bool>,
    pub(super) reconnect_completed: tokio::sync::watch::Sender<bool>,
    pub(super) reconnect_gate: Option<Arc<Notify>>,
}

struct Handler {
    calls: AtomicUsize,
    disposed: AtomicUsize,
    calls_at_dispose: AtomicUsize,
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
    /// Test gate for gated-event: the first non-final event waits here.
    event_gate: Arc<Notify>,
    gated_event: bool,
}
impl Handler {
    fn new(mode: &str) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            disposed: AtomicUsize::new(0),
            calls_at_dispose: AtomicUsize::new(usize::MAX),
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
            event_gate: Arc::new(Notify::new()),
            gated_event: mode == "gated-event",
        })
    }
}
impl HostHandler for Handler {
    fn dispose(&self) -> HostFuture<()> {
        self.calls_at_dispose
            .store(self.active_calls.load(Ordering::SeqCst), Ordering::SeqCst);
        self.disposed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
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
                response.insert("padding".into(), JsValue::String("x".repeat(2800).into()));
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
        let gate = (self.gated_event
            && event.get("final").and_then(JsValue::as_bool) != Some(true))
        .then(|| self.event_gate.clone());
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
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
        execution_timeout: Some(Duration::from_secs(2)),
        close_timeout: Duration::from_secs(1),
        ..Limits::default()
    }
}
// Protocol assertions should not race Python startup/IO on a loaded CI worker.
fn protocol_limits() -> Limits {
    Limits {
        handshake_timeout: Duration::from_secs(5),
        execution_timeout: Some(Duration::from_secs(10)),
        close_timeout: Duration::from_secs(5),
        ..limits()
    }
}

// External processes do not share Tokio's virtual clock. Keep a runnable task
// so Tokio cannot auto-advance while a fixture is still producing its event.
struct ManualClock(tokio::task::JoinHandle<()>);
impl ManualClock {
    fn start() -> Self {
        tokio::time::pause();
        Self(tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        }))
    }
}
impl Drop for ManualClock {
    fn drop(&mut self) {
        self.0.abort();
        tokio::time::resume();
    }
}
async fn wait_for_event(label: &str, ready: impl Fn() -> bool) {
    let watchdog = std::time::Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(
            std::time::Instant::now() < watchdog,
            "missing fixture event: {label}"
        );
        tokio::task::yield_now().await;
    }
}
struct Receipt(std::path::PathBuf);
impl Receipt {
    fn assert_pass(&self, mode: &str) {
        // The peer writes this after the host closes its stdin, so it may still be
        // in flight when the result arrives (#230); wait briefly before judging.
        let expected = format!("PASS {mode}\n");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut text = std::fs::read_to_string(&self.0).unwrap_or_default();
        while text != expected && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            text = std::fs::read_to_string(&self.0).unwrap_or_default();
        }
        assert_eq!(text, expected, "peer protocol assertions did not finish");
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
async fn zero_and_inconsistent_limits_are_rejected_before_spawn() {
    for field in [
        "handshake_timeout",
        "execution_timeout",
        "terminal_drain_timeout",
        "io_timeout",
        "close_timeout",
        "queue_capacity",
        "max_frame_bytes",
        "max_stderr_bytes",
        "max_module_calls",
        "max_buffered_bytes",
        "buffer_smaller_than_frame",
        "execution_timeout_unrepresentable",
    ] {
        let mut bounded = limits();
        match field {
            "handshake_timeout" => bounded.handshake_timeout = Duration::ZERO,
            "execution_timeout" => bounded.execution_timeout = Some(Duration::ZERO),
            "terminal_drain_timeout" => bounded.terminal_drain_timeout = Duration::ZERO,
            "io_timeout" => bounded.io_timeout = Duration::ZERO,
            "close_timeout" => bounded.close_timeout = Duration::ZERO,
            "queue_capacity" => bounded.queue_capacity = 0,
            "max_frame_bytes" => bounded.max_frame_bytes = 0,
            "max_stderr_bytes" => bounded.max_stderr_bytes = 0,
            "max_module_calls" => bounded.max_module_calls = 0,
            "max_buffered_bytes" => bounded.max_buffered_bytes = 0,
            "buffer_smaller_than_frame" => bounded.max_buffered_bytes = bounded.max_frame_bytes - 1,
            // The executable does not exist: a Protocol rejection proves the
            // checked_add validation ran before spawn rather than surfacing Io.
            "execution_timeout_unrepresentable" => {
                bounded.execution_timeout = Some(Duration::MAX);
            }
            _ => unreachable!(),
        }
        let error = HostClient::spawn(
            &mut Command::new("nonexistent-sidecar-for-limit-test"),
            None,
            bounded,
        )
        .await
        .err()
        .expect("invalid limits must fail before spawn");
        assert_eq!(error.kind, HostErrorKind::Protocol, "{field}: {error}");
        let reason = match field {
            "buffer_smaller_than_frame" => "must be at least max_frame_bytes",
            "execution_timeout_unrepresentable" => "must be representable",
            _ => "must be positive",
        };
        assert!(error.message.contains(reason), "{field}: {error}");
    }
    let mut boundary = limits();
    boundary.max_buffered_bytes = boundary.max_frame_bytes;
    boundary.validate().unwrap();
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
    bounded = protocol_limits();
    bounded.max_frame_bytes = 1024;
    let error = HostClient::spawn(&mut command("oversized"), None, bounded)
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, HostErrorKind::Limit);
    let execution_timeout = Duration::from_millis(100);
    bounded.execution_timeout = Some(execution_timeout);
    let client = HostClient::spawn(&mut command("callback-timeout"), None, bounded)
        .await
        .unwrap();
    let handler = Handler::new("callback-timeout");
    let clock = ManualClock::start();
    let task = tokio::spawn(client.execute(request(), handler.clone()));
    wait_for_event("pending module callback", || {
        handler.active_calls.load(Ordering::SeqCst) == 1
    })
    .await;
    assert!(!task.is_finished(), "timed out before callback started");
    tokio::time::advance(execution_timeout + Duration::from_millis(1)).await;
    // Resume the real clock only after the deadline is ready; child shutdown
    // still uses OS pipes, whose scheduling is independent of Tokio time.
    drop(clock);
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.kind, HostErrorKind::Timeout);
    assert!(error.accepted);
    assert_eq!(handler.active_calls.load(Ordering::SeqCst), 0);
    println!("HOST_REJECT callback-timeout: {error}");
}

#[tokio::test]
async fn default_execution_timeout_is_unbounded_past_former_watchdog() {
    let mode = "gated-event";
    let handler = Handler::new(mode);
    let (mut command, receipt) = peer("positive");
    let client = HostClient::spawn(&mut command, None, Limits::default())
        .await
        .unwrap();
    let clock = ManualClock::start();
    let task = tokio::spawn(client.execute(request(), handler.clone()));
    wait_for_event("first event blocked in handler", || {
        handler.events.load(Ordering::SeqCst) == 1
    })
    .await;
    // The old default was 300 s. With no configured watchdog, 301 s of virtual
    // time must leave a still-legitimate turn pending instead of truncating it.
    tokio::time::advance(Duration::from_secs(301)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(
        !task.is_finished(),
        "default execution timeout truncated a 301s turn"
    );
    handler.event_gate.notify_one();
    drop(clock);
    let result = timeout(Duration::from_secs(10), task)
        .await
        .expect("unbounded execution did not resume after releasing the event")
        .unwrap()
        .unwrap();
    assert!(matches!(result, ExecuteResult::Terminal(_)));
    receipt.assert_pass("positive");
    println!("HOST_PASS default-execution-timeout unbounded past 301s virtual time");
}

#[tokio::test]
async fn cancellation_without_terminal_exits_within_post_cancel_deadline() {
    for (mode, accepted, stream, last_applied) in [
        ("host-cancel-before-acceptance-hang", false, None, None),
        (
            "host-cancel-after-acceptance-hang",
            true,
            Some("stream"),
            Some(0.0),
        ),
        (
            "host-cancel-no-terminal-hang",
            true,
            Some("stream"),
            Some(0.0),
        ),
    ] {
        let (mut command, receipt) = peer(mode);
        let limits = Limits::default();
        let deadline = POST_CANCEL_EXIT_TIMEOUT;
        let client = HostClient::spawn(&mut command, None, limits).await.unwrap();
        let pid = client.connection.pid().expect("owned peer pid");
        let signals = client.test_signals.clone();
        let cancellation = Cancellation::default();
        let handler = Handler::new(mode);
        let clock = ManualClock::start();
        let task = tokio::spawn(client.execute_cancellable(
            request(),
            handler.clone(),
            cancellation.clone(),
        ));
        wait_for_event("fixture ready", || {
            receipt.0.with_extension("ready").exists()
        })
        .await;
        if accepted {
            // The event is delivered only after acceptance (it carries streamId).
            wait_for_event("first event applied", || {
                handler.applied_events.load(Ordering::SeqCst) == 1
            })
            .await;
        }
        let cancel_at = Instant::now();
        cancellation.cancel(Some("test_abort".into()));
        // Let the execute loop observe cancellation and register its timer.
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        assert!(
            !task.is_finished(),
            "{mode}: returned before the post-cancel deadline"
        );
        // Well inside the 5 s TS close grace: a silent peer must not cut the
        // wait short.
        tokio::time::advance(deadline - Duration::from_millis(100)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(
            !task.is_finished(),
            "{mode}: returned at 4.9s, before POST_CANCEL_EXIT_TIMEOUT"
        );
        // 5 s + 1 ms: silence past the bound is the only case that times out.
        tokio::time::advance(Duration::from_millis(101)).await;
        // Observe the real execute wrapper leaving run while time is STILL
        // paused at 5.001 s. A +1 s arming bug cannot pass this barrier.
        wait_for_event(mode, || signals.loop_exited.load(Ordering::SeqCst)).await;
        assert_eq!(Instant::now(), cancel_at + Duration::from_millis(5001));
        wait_for_event("peer entered close", || {
            receipt.0.with_extension("closing").exists()
        })
        .await;
        std::fs::write(receipt.0.with_extension("close-release"), b"release").unwrap();
        drop(clock);
        let error = timeout(Duration::from_secs(10), task)
            .await
            .unwrap_or_else(|_| panic!("{mode}: cancellation waited past its bounded exit"))
            .unwrap()
            .expect_err(mode);
        assert_eq!(error.kind, HostErrorKind::Timeout, "{mode}: {error}");
        assert_eq!(error.accepted, accepted, "{mode}: {error}");
        assert_eq!(
            error.stream_id,
            stream.map(JsString::from),
            "{mode}: {error}"
        );
        assert_eq!(error.last_applied_sequence, last_applied, "{mode}: {error}");
        assert_eq!(
            handler.disposed.load(Ordering::SeqCst),
            1,
            "{mode}: cleanup did not run"
        );
        receipt.assert_pass(mode);
        assert_reaped(pid).await;
        println!("HOST_CANCEL_DEADLINE_PASS {mode}: {error}");
    }
}

#[tokio::test]
async fn late_terminal_after_cancel_wins_over_post_cancel_deadline() {
    let mode = "host-cancel-late-terminal";
    let (mut command, receipt) = peer(mode);
    let client = HostClient::spawn(&mut command, None, Limits::default())
        .await
        .unwrap();
    let cancellation = Cancellation::default();
    let handler = Handler::new(mode);
    let clock = ManualClock::start();
    let task =
        tokio::spawn(client.execute_cancellable(request(), handler.clone(), cancellation.clone()));
    wait_for_event("first event applied", || {
        handler.applied_events.load(Ordering::SeqCst) == 1
    })
    .await;
    cancellation.cancel(Some("test_abort".into()));
    // Let the execute loop observe cancellation and register its timer.
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    // The peer holds its authoritative terminal for 3 virtual seconds. TS has
    // no post-cancel timer, so this late terminal is legitimate; the 5 s bound
    // must not turn it into Timeout.
    tokio::time::advance(Duration::from_secs(3)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(
        !task.is_finished(),
        "{mode}: cancelled execute returned before the late terminal"
    );
    std::fs::write(receipt.0.with_extension("release"), b"release\n").unwrap();
    // If the bound is too short, the cancelled execute returns Timeout before
    // the peer's 3 s terminal can be applied. Poll briefly, then resume the
    // clock (child shutdown needs it) and report the actual settle result
    // instead of a bare watchdog.
    let watchdog = std::time::Instant::now() + Duration::from_secs(5);
    while handler.applied_events.load(Ordering::SeqCst) != 2 && std::time::Instant::now() < watchdog
    {
        tokio::task::yield_now().await;
    }
    drop(clock);
    if handler.applied_events.load(Ordering::SeqCst) != 2 {
        let outcome = timeout(Duration::from_secs(10), task)
            .await
            .expect("cancelled execute did not settle after the late terminal");
        panic!(
            "{mode}: cancelled execute settled as {outcome:?} instead of applying the 3 s terminal"
        );
    }
    let result = timeout(Duration::from_secs(10), task)
        .await
        .expect("late terminal did not win over the post-cancel exit deadline")
        .unwrap()
        .expect(mode);
    assert!(
        matches!(result, ExecuteResult::Terminal(_)),
        "{mode}: expected the authoritative terminal, got {result:?}"
    );
    assert_eq!(
        handler.disposed.load(Ordering::SeqCst),
        1,
        "{mode}: cleanup did not run"
    );
    receipt.assert_pass(mode);
    println!("HOST_PASS {mode}: terminal arriving 3s after cancel won inside the 5s bound");
}

#[test]
fn post_cancel_exit_timeout_matches_ts_close_grace() {
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../tests/fixtures/host-cancel-timeout/oracle.json"
    ));
    let expected = oracle
        .get("defaultKillTimeoutMs")
        .and_then(JsValue::as_number)
        .expect("oracle defaultKillTimeoutMs");
    assert_eq!(
        POST_CANCEL_EXIT_TIMEOUT.as_millis() as f64,
        expected,
        "POST_CANCEL_EXIT_TIMEOUT must equal TS's child-close grace"
    );
}

#[test]
#[ignore = "requires PDRS_BASELINE and baseline node_modules"]
fn host_cancel_timeout_fixture_regenerates_identically_from_ts() {
    crate::test_baseline::regenerates_identically("host-cancel-timeout");
}

#[test]
fn default_limits_disable_execution_watchdog() {
    assert_eq!(Limits::default().execution_timeout, None);
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
    // Test the grace deadlines themselves, while letting the external process
    // finish each observation before advancing virtual time. Python does not
    // share Tokio's clock; real grace periods race its scheduling (#308).
    // close_timeout also bounds the final reap after SIGKILL, which is plain
    // OS scheduling and cannot be driven by the paused clock. Two seconds keep
    // that observation honest under load; the deadline value is otherwise
    // symbolic because the manual clock drives every grace phase (#528).
    let mut bounded = protocol_limits();
    bounded.close_timeout = Duration::from_secs(2);
    let (mut command, receipt) = peer("close-hang");
    let client = HostClient::spawn(&mut command, None, bounded)
        .await
        .unwrap();
    let pid = client.child_id().unwrap();
    let writer = client.connection.writer_task();
    let term_sent = client.connection.term_sent();
    let clock = ManualClock::start();
    let close = client.close();
    tokio::pin!(close);
    tokio::select! {
        biased;
        result = &mut close => panic!("close skipped EOF grace period: {result:?}"),
        _ = wait_for_event("stdin EOF and writer joined", || {
            std::fs::read_to_string(receipt.0.with_extension("eof")).ok().as_deref() == Some("EOF\n")
                && writer.is_finished()
        }) => {}
    }
    // Now the writer join is ready, so this poll arms the EOF grace timer.
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(close.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    // Advancing once does not guarantee the timer driver has dispatched the
    // deadline. Drive bounded ticks until the actual TERM attempt; then freeze
    // again so Python gets its full event-observation window.
    let tick = Duration::from_millis(5);
    for _ in 0..=(bounded.close_timeout.as_millis() / 5 + 2) as usize {
        tokio::time::advance(tick).await;
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(close.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        if term_sent.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        term_sent.load(Ordering::SeqCst),
        1,
        "TERM deadline did not fire"
    );
    tokio::select! {
        biased;
        result = &mut close => panic!("close skipped TERM grace period: {result:?}"),
        _ = wait_for_event("SIGTERM", || {
            std::fs::read_to_string(receipt.0.with_extension("term")).ok().as_deref() == Some("TERM\n")
        }) => {}
    }
    // The TERM timer is armed by the poll which sent the signal. Keep it
    // frozen until the fixture has persisted the TERM event, then allow KILL.
    tokio::time::advance(bounded.close_timeout + Duration::from_millis(1)).await;
    drop(clock);
    let result = timeout(Duration::from_secs(15), &mut close)
        .await
        .expect("close did not finish after SIGKILL");
    result.unwrap();
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
    let timed = matches!(mode, "cancel-timeout" | "cancel-before-timeout");
    let mut bounded = protocol_limits();
    if timed {
        bounded.execution_timeout = Some(Duration::from_millis(400));
    }
    let client = HostClient::spawn(&mut command, None, bounded)
        .await
        .unwrap();
    let clock = timed.then(ManualClock::start);
    let task = tokio::spawn(client.execute_cancellable(request(), handler.clone(), cancellation));
    if timed {
        // Compare persisted text, not mere existence: Python's write_text is
        // not atomic, so exists() can win the race against the content (#308).
        let expected = if mode == "cancel-before-timeout" {
            "module response received\n"
        } else {
            "ACK and cancel received\n"
        };
        wait_for_event("cancel protocol ready", || {
            std::fs::read_to_string(receipt.0.with_extension("ready"))
                .ok()
                .as_deref()
                == Some(expected)
        })
        .await;
        assert!(
            !task.is_finished(),
            "execution timed out before protocol assertions"
        );
        tokio::time::advance(Duration::from_millis(401)).await;
    }
    drop(clock);
    let result = task.await.unwrap();
    if result.is_ok() || matches!(mode, "cancel-before-disconnect" | "cancel-before-timeout") {
        receipt.assert_pass(mode);
    }
    (result, handler)
}

#[tokio::test]
async fn accepted_then_final_failed_response_matches_ts_rejection() {
    let mode = "accepted-then-rejected";
    let handler = Handler::new(mode);
    let (mut command, receipt) = peer(mode);
    let client = HostClient::spawn(&mut command, None, limits())
        .await
        .unwrap();
    let result = client.execute(request(), handler.clone()).await.unwrap();
    let ExecuteResult::Rejected(raw) = result else {
        panic!("expected TS rejection")
    };
    assert!(text_is(raw.get("outcome"), "failed"));
    assert_eq!(handler.applied_events.load(Ordering::SeqCst), 1);
    receipt.assert_pass(mode);
    println!("TS_REJECTION_PASS acceptance -> applied event 0/ACK -> final failed response");
}

#[tokio::test]
async fn unbounded_execution_and_maximum_terminal_drain_do_not_panic() {
    for bounded in [
        Limits {
            execution_timeout: None,
            ..limits()
        },
        Limits {
            terminal_drain_timeout: Duration::MAX,
            ..limits()
        },
    ] {
        let (mut command, receipt) = peer("positive");
        let client = HostClient::spawn(&mut command, None, bounded)
            .await
            .unwrap();
        let result = timeout(
            Duration::from_secs(2),
            client.execute(request(), Handler::new("positive")),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(result, ExecuteResult::Terminal(_)));
        receipt.assert_pass("positive");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn close_allows_sigterm_cleanup_after_stdin_eof() {
    let mode = "close-term-cleanup";
    let (mut command, receipt) = peer(mode);
    let bounded = protocol_limits();
    let client = HostClient::spawn(&mut command, None, bounded)
        .await
        .unwrap();
    let pid = client.child_id().unwrap();
    timeout(Duration::from_secs(15), client.close())
        .await
        .unwrap()
        .unwrap();
    receipt.assert_pass(mode);
    assert_reaped(pid).await;
    println!(
        "CLOSE_TERM_PASS stdin EOF observed before SIGTERM; cleanup receipt persisted; child reaped"
    );
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
            matches!(result.unwrap(), ExecuteResult::Aborted { terminal: None }),
            "{mode}"
        );
        assert_eq!(
            handler.events.load(Ordering::SeqCst),
            1,
            "incomplete final must not reach the handler: {mode}"
        );
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
async fn cancel_before_disconnect_never_writes_pre_acceptance_bytes() {
    let (result, _) = cancel_run("cancel-before-disconnect").await;
    let error = result.unwrap_err();
    assert_eq!(error.kind, HostErrorKind::Disconnected);
    assert!(!error.accepted);
    assert!(!error.message.contains("AssertionError"), "{error}");
    // cancel_run also requires the receipt written only after the quiet window.
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
    let execution_timeout = Duration::from_millis(200);
    let bounded = Limits {
        terminal_drain_timeout: Duration::from_millis(300),
        execution_timeout: Some(execution_timeout),
        ..protocol_limits()
    };
    let client = HostClient::spawn(&mut command, None, bounded)
        .await
        .unwrap();
    let handler = Handler::new(mode);
    let clock = ManualClock::start();
    let execute = client.execute(request(), handler.clone());
    tokio::pin!(execute);
    tokio::select! {
        biased;
        result = &mut execute => panic!("finished before terminal drain: {result:?}"),
        _ = wait_for_event("terminal applied with pending callback", || {
            handler.applied_events.load(Ordering::SeqCst) == 1
                && handler.active_calls.load(Ordering::SeqCst) == 1
        }) => {}
    }
    // Consume the completed event and register its drain timer before moving
    // the virtual clock. Merely observing the handler's counter is too early.
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(execute.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    tokio::time::advance(execution_timeout + Duration::from_millis(1)).await;
    std::future::poll_fn(|cx| {
        assert!(
            std::future::Future::poll(execute.as_mut(), cx).is_pending(),
            "known terminal must survive execution timeout and wait for drain"
        );
        std::task::Poll::Ready(())
    })
    .await;
    tokio::time::advance(bounded.terminal_drain_timeout - execution_timeout).await;
    let finished = tokio::select! {
        biased;
        result = &mut execute => Some(result),
        _ = wait_for_event("terminal drain aborted callback", || {
            handler.active_calls.load(Ordering::SeqCst) == 0
        }) => None
    };
    // stop_child polls the OS with real-time sleeps. Only the drain assertion
    // needs virtual time; resume before awaiting pipe flush and child reaping.
    drop(clock);
    let result = match finished {
        Some(result) => result,
        None => timeout(Duration::from_secs(10), &mut execute)
            .await
            .expect("transport cleanup watchdog"),
    }
    .unwrap();
    assert!(matches!(result, ExecuteResult::Terminal(_)));
    assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
    assert_eq!(handler.active_calls.load(Ordering::SeqCst), 0);
    assert_eq!(handler.events.load(Ordering::SeqCst), 1);
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
            _ => {
                bounded.max_buffered_bytes = 3000;
                bounded.max_frame_bytes = 3000;
            }
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
        let mut bounded = protocol_limits();
        if mode == "grandchild-timeout" {
            bounded.execution_timeout = Some(Duration::from_millis(100));
        }
        let client = HostClient::spawn(&mut command, None, bounded)
            .await
            .unwrap();
        let pid: u32 = std::fs::read_to_string(receipt.0.with_extension("pid"))
            .unwrap()
            .parse()
            .unwrap();
        if mode == "grandchild-timeout" {
            let handler = Handler::new(mode);
            let clock = ManualClock::start();
            let task = tokio::spawn(client.execute(request(), handler.clone()));
            wait_for_event("accepted callback", || {
                handler.active_calls.load(Ordering::SeqCst) != 0
            })
            .await;
            assert!(!task.is_finished());
            tokio::time::advance(Duration::from_millis(101)).await;
            drop(clock);
            let error = task.await.unwrap().unwrap_err();
            assert_eq!(error.kind, HostErrorKind::Timeout);
            assert!(error.accepted);
            assert_eq!(handler.active_calls.load(Ordering::SeqCst), 0);
        } else if mode == "grandchild-drop" {
            drop(client);
        } else {
            client.close().await.unwrap();
        }
        assert_reaped(pid).await;
        println!("HOST_GROUP_PASS {mode}: grandchild={pid}");
    }
}

/// #265-1 measurement: dropping a `TurnScope` after its captured runtime has
/// shut down must not panic in `Drop`. Tokio's `Handle::spawn` on a shut-down
/// runtime cancels the task instead of panicking, so cleanup is skipped
/// (documented on `HostHandler::dispose`) rather than aborting the process.
#[test]
fn turn_scope_drop_after_runtime_shutdown_does_not_panic() {
    let handler = Handler::new("ok");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let scope = runtime.block_on(async {
        let mut scope = execution::TurnScope::new(handler.clone(), Duration::from_secs(1));
        scope.spawn_pending_callback_for_test();
        scope
    });
    drop(runtime);
    let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        drop(scope);
    }));
    assert!(dropped.is_ok(), "TurnScope::drop panicked after shutdown");
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 0);
}

/// A handler that records what the `validate_event` hook observed and can be
/// told to fail on one payload type, for testing the hook's own contract.
struct RecordingHook {
    seen: std::sync::Mutex<Vec<String>>,
    applied: AtomicUsize,
    disposed: AtomicUsize,
    fail_on: Option<&'static str>,
}
impl Default for RecordingHook {
    fn default() -> Self {
        Self {
            seen: std::sync::Mutex::new(Vec::new()),
            applied: AtomicUsize::new(0),
            disposed: AtomicUsize::new(0),
            fail_on: None,
        }
    }
}
impl HostHandler for RecordingHook {
    fn dispose(&self) -> HostFuture<()> {
        self.disposed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn module_call(&self, _call: JsValue) -> HostFuture<JsObject> {
        Box::pin(async { Ok(JsObject::new()) })
    }
    fn event(&self, _event: JsValue) -> HostFuture<()> {
        self.applied.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn validate_event(&self, payload: &JsValue) -> Result<(), HostError> {
        let name = String::from_utf16_lossy(
            payload
                .get("type")
                .and_then(JsValue::as_string)
                .expect("validated event payload type")
                .units(),
        );
        self.seen.lock().unwrap().push(name.clone());
        if self.fail_on == Some(name.as_str()) {
            return Err(HostError::protocol(format!("test hook rejected {name}")));
        }
        Ok(())
    }
}
async fn run_with(mode: &str, handler: Arc<dyn HostHandler>) -> Result<ExecuteResult, HostError> {
    let (mut command, receipt) = peer(mode);
    let client = HostClient::spawn(&mut command, Some(&"fixture".into()), limits()).await?;
    let result = client.execute(request(), handler).await;
    if result.is_ok() {
        receipt.assert_pass(mode);
    }
    result
}
#[tokio::test]
async fn validate_event_hook_runs_after_suppression_and_skips_finals() {
    // Mode "turn-completed" streams text_delta(0), the sidecar's own
    // turn_completed(1) which the host suppresses as non-authoritative, then
    // text_delta(2) and the final(3). The hook must see exactly the two
    // text_delta payloads: the suppressed event never reaches it and the
    // authoritative final is projected by the caller instead. The peer's ack
    // assertion (lastAppliedSequence 2) proves ACK still follows application.
    let handler = Arc::new(RecordingHook::default());
    let result = run_with("turn-completed", handler.clone()).await.unwrap();
    assert!(matches!(result, ExecuteResult::Terminal(_)));
    assert_eq!(
        *handler.seen.lock().unwrap(),
        ["text_delta", "text_delta"],
        "hook must observe wire-received non-final events after suppression"
    );
    // text_delta(0), text_delta(2), and the final(3) are all applied — the
    // final carries the result and messages. Only the suppressed
    // turn_completed(1) never reaches the handler.
    assert_eq!(handler.applied.load(Ordering::SeqCst), 3);
    println!("HOST_PASS validate_event ordering: suppressed and final events skipped");
}
#[tokio::test]
async fn validate_event_failure_fails_execution_before_application() {
    // A hook rejection fails the execution closed: the violating event is
    // never applied and never ACKed. The peer stays stuck waiting for the
    // ack of sequence 0, so no receipt is written on the error path.
    let handler = Arc::new(RecordingHook {
        fail_on: Some("text_delta"),
        ..RecordingHook::default()
    });
    let result = run_with("positive", handler.clone()).await;
    let error = result.expect_err("hook rejection must fail the execution");
    assert_eq!(error.kind, HostErrorKind::Protocol);
    assert!(error.message.contains("test hook rejected text_delta"));
    assert_eq!(
        handler.applied.load(Ordering::SeqCst),
        0,
        "a violating event must not be applied to the handler"
    );
}
#[tokio::test]
async fn validate_event_rejection_is_never_acked_on_the_wire() {
    // ACK receipt for the rejection path: the peer's dedicated mode records
    // every host frame it actually receives until its stdin EOF, so "a
    // violating event is never ACKed" is asserted from the peer's own record
    // rather than inferred from the host's error return. Limitation: it only
    // sees frames the host wrote before closing; an ACK queued in the last
    // synchronous step before the error return and never flushed would not
    // show up here, so this is not a full wire receipt.
    let mode = "ack-eof-recorder";
    let handler = Arc::new(RecordingHook {
        fail_on: Some("text_delta"),
        ..RecordingHook::default()
    });
    let (mut command, receipt) = peer(mode);
    let client = HostClient::spawn(&mut command, Some(&"fixture".into()), limits())
        .await
        .unwrap();
    let pid = client.child_id().expect("owned peer pid");
    let result = client.execute(request(), handler.clone()).await;
    let error = result.expect_err("hook rejection must fail the execution");
    assert_eq!(error.kind, HostErrorKind::Protocol);
    assert!(error.message.contains("test hook rejected text_delta"));
    assert_eq!(
        handler.applied.load(Ordering::SeqCst),
        0,
        "a violating event must not be applied to the handler"
    );
    assert_eq!(
        handler.disposed.load(Ordering::SeqCst),
        1,
        "cleanup must dispose the handler exactly once"
    );
    // The peer writes its record after the host closes its stdin, so it may
    // still be in flight when the error arrives; wait briefly before judging.
    let record = receipt.0.with_extension("acks");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut text = std::fs::read_to_string(&record).unwrap_or_default();
    while text.is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        text = std::fs::read_to_string(&record).unwrap_or_default();
    }
    assert_eq!(
        text.trim(),
        "[]",
        "peer received host frames for a validation-rejected event: {text}"
    );
    assert_reaped(pid).await;
    println!("HOST_REJECT {mode}: wire ACK record empty; applied=0; disposed=1; child reaped");
}
#[tokio::test]
async fn ack_eof_recorder_records_a_real_ack() {
    // Positive control for `validate_event_rejection_is_never_acked_on_the_wire`:
    // the same recorder must see the ACK the host sends for an applied event,
    // otherwise an always-empty record would make that negative test vacuous.
    let mode = "ack-eof-recorder-positive";
    let handler = Arc::new(RecordingHook::default());
    let (mut command, receipt) = peer(mode);
    let client = HostClient::spawn(&mut command, Some(&"fixture".into()), limits())
        .await
        .unwrap();
    let pid = client.child_id().expect("owned peer pid");
    client
        .execute(request(), handler.clone())
        .await
        .expect("an applied event and a final must complete the execution");
    let record = receipt.0.with_extension("acks");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut text = std::fs::read_to_string(&record).unwrap_or_default();
    while text.is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        text = std::fs::read_to_string(&record).unwrap_or_default();
    }
    let frames: serde_json::Value = serde_json::from_str(text.trim()).expect("ack record JSON");
    assert!(
        frames.as_array().is_some_and(|frames| frames
            .iter()
            .any(|frame| frame["method"] == "ack" && frame["lastAppliedSequence"] == 0)),
        "recorder missed the host ACK for sequence 0: {text}"
    );
    assert_reaped(pid).await;
}
