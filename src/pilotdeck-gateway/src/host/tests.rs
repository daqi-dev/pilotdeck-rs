use super::*;
use pilotdeck_protocol::parse_json;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::{sync::Notify, time::Instant};

struct Handler {
    calls: AtomicUsize,
    events: AtomicUsize,
    release: Arc<Notify>,
    wait_for_event: bool,
    never_complete: bool,
    slow_event: bool,
    active_calls: Arc<AtomicUsize>,
}
impl Handler {
    fn new(mode: &str) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            active_calls: Arc::new(AtomicUsize::new(0)),
            events: AtomicUsize::new(0),
            release: Arc::new(Notify::new()),
            wait_for_event: mode == "callback-progress",
            never_complete: matches!(
                mode,
                "callback-disconnect" | "callback-timeout" | "duplicate-conflict"
            ),
            slow_event: mode == "slow-event",
        })
    }
}
impl HostHandler for Handler {
    fn module_call(&self, _call: JsValue) -> HostFuture<JsObject> {
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
            Ok(JsObject::new())
        })
    }
    fn event(&self, _event: JsValue) -> HostFuture<()> {
        self.events.fetch_add(1, Ordering::SeqCst);
        self.release.notify_one();
        let slow = self.slow_event;
        Box::pin(async move {
            if slow {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
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
        close_timeout: Duration::from_millis(50),
        ..Limits::default()
    }
}
fn peer(mode: &str) -> (Command, std::path::PathBuf) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../_tmp/20261017-a61/test-receipts");
    std::fs::create_dir_all(&dir).unwrap();
    let owned = dir.join(format!(
        "{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    // Exclusive directory: PID reuse can never turn an old PASS into evidence.
    std::fs::create_dir(&owned).unwrap();
    let receipt = owned.join(format!("{mode}.txt"));
    let mut command = Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/a6-host/fixture.py"
        ))
        .arg(mode);
    command.arg(&receipt);
    (command, receipt)
}
fn command(mode: &str) -> Command {
    peer(mode).0
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
        assert_eq!(
            std::fs::read_to_string(receipt).unwrap(),
            format!("PASS {mode}\n"),
            "peer protocol assertions did not finish"
        );
    }
    result
}

#[tokio::test]
async fn handshake_execute_ack_and_terminal() {
    for mode in [
        "positive",
        "slow-event",
        "no-ack",
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
            started.elapsed() < Duration::from_secs(2),
            "unbounded cleanup: {mode}"
        );
        println!("HOST_PASS {mode}");
    }
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
async fn invalid_frames_sequences_and_terminal_never_succeed() {
    for (mode, reason) in [
        ("sequence", "sequence mismatch"),
        ("before-accepted", "before acceptance"),
        ("bad-session", "active session"),
        ("wrong-call-conflict", "conflicting immutable identity"),
        (
            "event-generation",
            "Connection binding belongs to transport context",
        ),
        ("bad-terminal", "outcome conflicts"),
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
        assert_eq!(
            std::fs::read_to_string(receipt).unwrap(),
            format!("PASS {mode}\n")
        );
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
