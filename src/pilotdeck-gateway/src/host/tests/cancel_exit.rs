//! Cancel-state interleavings: real owned pipes plus a frozen monotonic clock.
use super::*;
use std::sync::Mutex;

fn cancel_peer(mode: &str) -> (Command, Receipt) {
    let (_, receipt) = peer(mode);
    let mut command = Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/host-cancel-timeout/peer.py"
        ))
        .arg(mode)
        .arg(&receipt.0);
    (command, receipt)
}
fn release(receipt: &Receipt, name: &str) {
    std::fs::write(receipt.0.with_extension(name), b"release").unwrap();
}
async fn marker(receipt: &Receipt, name: &str) {
    wait_for_event(name, || receipt.0.with_extension(name).exists()).await;
}
async fn assert_peer_reaped(receipt: &Receipt) {
    for generation in 1..=2 {
        if let Ok(pid) =
            std::fs::read_to_string(receipt.0.with_extension(format!("pid-{generation}")))
        {
            assert_reaped(pid.parse().unwrap()).await;
        }
    }
}
// Retained host observations stay visible even when published before the wait.
// A scheduler yield count cannot establish these state transitions.
async fn checkpoint<T: Clone>(
    label: &str,
    signal: &tokio::sync::watch::Sender<T>,
    ready: impl Fn(&T) -> bool,
) -> T {
    // The wall-clock watchdog diagnoses a missing checkpoint only. Semantic
    // time stays frozen until the host has published the requested state.
    wait_for_event(label, || ready(&signal.borrow())).await;
    signal.borrow().clone()
}
async fn cancel_and_arm(cancellation: &Cancellation, signals: &ExecutionSignals) -> Instant {
    let anchor = Instant::now();
    cancellation.cancel(None);
    let deadline = checkpoint(
        "cancel deadline armed",
        &signals.cancel_deadline,
        Option::is_some,
    )
    .await
    .unwrap();
    assert_eq!(deadline, anchor + POST_CANCEL_EXIT_TIMEOUT);
    anchor
}
async fn boundary(signals: &ExecutionSignals, anchor: Instant, receipt: &Receipt) {
    tokio::time::advance(anchor + Duration::from_millis(4900) - Instant::now()).await;
    // Give an incorrectly early timer the same opportunity to fail the
    // pre-deadline assertion as before; arming/IO never rely on these yields.
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert!(
        !signals.loop_exited.load(Ordering::SeqCst),
        "execute loop left before 5 s"
    );
    tokio::time::advance(Duration::from_millis(101)).await;
    wait_for_event("execute loop exited at 5.001 s", || {
        signals.loop_exited.load(Ordering::SeqCst)
    })
    .await;
    assert_eq!(Instant::now(), anchor + Duration::from_millis(5001));
    // Observe actual child close too, without giving either timer extra time.
    marker(receipt, "closing").await;
}

#[tokio::test]
async fn cancellation_before_and_during_reconnect_uses_original_absolute_deadline() {
    for before in [true, false] {
        for mode in ["pending-hello", "pending-resume", "silent-resume"] {
            let (command, receipt) = cancel_peer(mode);
            let mut client = HostClient::spawn_resumable(command, None, Limits::default())
                .await
                .unwrap();
            let original_pid = client.connection.pid().expect("owned original peer");
            client.test_signals.reconnect_gate = Some(Arc::new(Notify::new()));
            let signals = client.test_signals.clone();
            let handler = Handler::new(mode);
            let cancellation = Cancellation::default();
            let clock = ManualClock::start();
            let task = tokio::spawn(client.execute_cancellable(
                request(),
                handler.clone(),
                cancellation.clone(),
            ));
            marker(&receipt, "ready").await;
            let anchor = Instant::now();
            if before {
                assert_eq!(cancel_and_arm(&cancellation, &signals).await, anchor);
                tokio::time::advance(Duration::from_secs(2)).await;
            }
            release(&receipt, "disconnect-release");
            checkpoint(
                "host reconnect started",
                &signals.reconnect_started,
                |started| *started,
            )
            .await;
            // stdout EOF can precede OS exit. close() polls WNOWAIT every
            // 5 virtual ms; freezing before the child exits would strand that
            // poll forever. Hold reconnect until real exit is observable, then
            // let the unchanged close/handshake run at the SAME virtual instant.
            let leader = rustix::process::Pid::from_raw(original_pid as i32).unwrap();
            assert!(!crate::process_group::leader_exited_unreaped(leader).unwrap());
            release(&receipt, "original-exit-release");
            wait_for_event("original peer exited without reaping", || {
                crate::process_group::leader_exited_unreaped(leader).unwrap()
            })
            .await;
            signals.reconnect_gate.as_ref().unwrap().notify_one();
            marker(&receipt, "replacement-ready").await;
            if !before {
                assert_eq!(cancel_and_arm(&cancellation, &signals).await, anchor);
                tokio::time::advance(Duration::from_secs(2)).await;
            }
            if mode != "pending-hello" {
                release(&receipt, "handshake-release");
                marker(&receipt, "resume-ready").await;
                // Spend another second inside the same reconnect await. A
                // replacement that resumes then stays silent must not rearm.
                tokio::time::advance(Duration::from_secs(1)).await;
                if mode == "silent-resume" {
                    release(&receipt, "resume-release");
                    marker(&receipt, "resumed").await;
                    checkpoint(
                        "host reconnect completed",
                        &signals.reconnect_completed,
                        |completed| *completed,
                    )
                    .await;
                }
            }
            boundary(&signals, anchor, &receipt).await;
            assert!(!task.is_finished(), "close was not gated");
            release(&receipt, "close-release");
            drop(clock);
            let error = timeout(Duration::from_secs(10), task)
                .await
                .expect("cleanup stuck")
                .unwrap()
                .unwrap_err();
            assert_eq!(error.kind, HostErrorKind::Timeout, "{mode}: {error}");
            assert!(error.accepted);
            assert_eq!(error.last_applied_sequence, Some(0.0));
            assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
            receipt.assert_pass(mode);
            assert_peer_reaped(&receipt).await;
            println!(
                "CANCEL_RECONNECT_PASS before={before} peer={mode} loop_exit=5.001s disposed=1"
            );
        }
    }
}

struct ApplicationHandler {
    block_sequence: Option<u64>,
    block_module: bool,
    events: AtomicUsize,
    applied: Arc<Mutex<Vec<u64>>>,
    active: Arc<AtomicUsize>,
    gate: Arc<Notify>,
    disposed: AtomicUsize,
}
impl ApplicationHandler {
    fn new(mode: &str) -> Arc<Self> {
        Arc::new(Self {
            block_sequence: match mode {
                "terminal-first-stall" => Some(0),
                "terminal-final-stall" => Some(1),
                _ => None,
            },
            block_module: mode == "terminal-module-stall",
            events: AtomicUsize::new(0),
            applied: Arc::new(Mutex::new(Vec::new())),
            active: Arc::new(AtomicUsize::new(0)),
            gate: Arc::new(Notify::new()),
            disposed: AtomicUsize::new(0),
        })
    }
}
impl HostHandler for ApplicationHandler {
    fn event(&self, event: JsValue) -> HostFuture<()> {
        let sequence = event.get("sequence").and_then(JsValue::as_number).unwrap() as u64;
        self.events.fetch_add(1, Ordering::SeqCst);
        let block = self.block_sequence == Some(sequence);
        let gate = self.gate.clone();
        let applied = self.applied.clone();
        let active = self.active.clone();
        Box::pin(async move {
            active.fetch_add(1, Ordering::SeqCst);
            let _guard = ActiveCall(active);
            if block {
                gate.notified().await;
            }
            applied.lock().unwrap().push(sequence);
            Ok(())
        })
    }
    fn module_call(&self, _: JsValue) -> HostFuture<JsObject> {
        let block = self.block_module;
        let active = self.active.clone();
        Box::pin(async move {
            active.fetch_add(1, Ordering::SeqCst);
            let _guard = ActiveCall(active);
            if block {
                std::future::pending::<()>().await;
            }
            Ok(JsObject::new())
        })
    }
    fn dispose(&self) -> HostFuture<()> {
        assert_eq!(self.active.load(Ordering::SeqCst), 0);
        self.disposed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn cancellation_bounds_pending_terminal_application_and_preserves_known_terminal() {
    for (mode, cancel_after_final) in [
        ("terminal-first-stall", false),
        ("terminal-final-stall", false),
        ("terminal-module-stall", false),
        ("terminal-first-stall", true),
        ("terminal-final-stall", true),
        ("terminal-module-stall", true),
    ] {
        let (mut command, receipt) = cancel_peer(mode);
        // Force the cancel bound, rather than the shorter terminal drain, to
        // stop a pending module after final application as well.
        let limits = Limits {
            terminal_drain_timeout: Duration::from_secs(60),
            ..Limits::default()
        };
        let client = HostClient::spawn(&mut command, None, limits).await.unwrap();
        let signals = client.test_signals.clone();
        let handler = ApplicationHandler::new(mode);
        let cancellation = Cancellation::default();
        let clock = ManualClock::start();
        let task = tokio::spawn(client.execute_cancellable(
            request(),
            handler.clone(),
            cancellation.clone(),
        ));
        marker(&receipt, "ready").await;
        wait_for_event("first callback started", || {
            handler.events.load(Ordering::SeqCst) == 1
        })
        .await;
        if cancel_after_final {
            release(&receipt, "terminal-release");
            wait_for_event("final validated before cancellation", || {
                signals.final_received.load(Ordering::SeqCst)
            })
            .await;
        }
        let anchor = cancel_and_arm(&cancellation, &signals).await;
        if !cancel_after_final {
            tokio::time::advance(Duration::from_secs(3)).await;
            release(&receipt, "terminal-release");
        }
        wait_for_event("validated final queued", || {
            signals.final_received.load(Ordering::SeqCst)
        })
        .await;
        if mode != "terminal-first-stall" {
            wait_for_event("final callback started", || {
                handler.events.load(Ordering::SeqCst) == 2
            })
            .await;
        }
        boundary(&signals, anchor, &receipt).await;
        assert_eq!(
            handler.active.load(Ordering::SeqCst),
            0,
            "callback was not dropped"
        );
        release(&receipt, "close-release");
        drop(clock);
        let result = timeout(Duration::from_secs(10), task)
            .await
            .expect("cleanup stuck")
            .unwrap()
            .unwrap();
        let ExecuteResult::Terminal(terminal) = result else {
            panic!("lost validated terminal: {result:?}");
        };
        assert_eq!(
            terminal.get("sequence").and_then(JsValue::as_number),
            Some(1.0)
        );
        let expected = match mode {
            "terminal-first-stall" => vec![],
            "terminal-final-stall" => vec![0],
            _ => vec![0, 1],
        };
        assert_eq!(*handler.applied.lock().unwrap(), expected);
        let last_applied = expected.last().map_or(-1.0, |sequence| *sequence as f64);
        assert_eq!(
            f64::from_bits(signals.last_applied_bits.load(Ordering::SeqCst)),
            last_applied
        );
        assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
        receipt.assert_pass(mode);
        assert_peer_reaped(&receipt).await;
        println!(
            "CANCEL_TERMINAL_PASS {mode} cancel_after_final={cancel_after_final} loop_exit=5.001s terminal=1 applied={expected:?} disposed=1"
        );
    }
}

#[tokio::test]
async fn cancellation_terminal_application_released_inside_bound_finishes_normally() {
    for mode in ["terminal-first-stall", "terminal-final-stall"] {
        let (mut command, receipt) = cancel_peer(mode);
        let client = HostClient::spawn(&mut command, None, Limits::default())
            .await
            .unwrap();
        let signals = client.test_signals.clone();
        let handler = ApplicationHandler::new(mode);
        let cancellation = Cancellation::default();
        let clock = ManualClock::start();
        let task = tokio::spawn(client.execute_cancellable(
            request(),
            handler.clone(),
            cancellation.clone(),
        ));
        marker(&receipt, "ready").await;
        let anchor = cancel_and_arm(&cancellation, &signals).await;
        tokio::time::advance(Duration::from_secs(3)).await;
        release(&receipt, "terminal-release");
        wait_for_event("final validated", || {
            signals.final_received.load(Ordering::SeqCst)
        })
        .await;
        assert!(!signals.loop_exited.load(Ordering::SeqCst));
        handler.gate.notify_one();
        wait_for_event("execute returned applied terminal", || {
            signals.loop_exited.load(Ordering::SeqCst)
        })
        .await;
        assert_eq!(Instant::now(), anchor + Duration::from_secs(3));
        marker(&receipt, "closing").await;
        release(&receipt, "close-release");
        drop(clock);
        let result = timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(result, ExecuteResult::Terminal(_)));
        assert_eq!(*handler.applied.lock().unwrap(), vec![0, 1]);
        assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
        receipt.assert_pass(mode);
        assert_peer_reaped(&receipt).await;
    }
}
