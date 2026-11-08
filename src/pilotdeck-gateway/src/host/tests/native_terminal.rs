use super::*;

fn native_peer() -> Command {
    let mut command = Command::new("python3");
    command.arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/host-native-terminal/peer.py"));
    command
}

#[tokio::test]
async fn native_failure_waits_for_the_whole_validated_event_prefix() {
    let handler = Handler::new("gated-event");
    let mut limits = limits();
    limits.terminal_drain_timeout = Duration::from_secs(5);
    let client = HostClient::spawn(&mut native_peer(), Some(&"fixture".into()), limits)
        .await.unwrap().with_native_terminal_errors();
    let final_received = client.final_received();
    let worker = tokio::spawn(client.execute(request(), handler.clone()));
    wait_for_event("native final received", || final_received.load(Ordering::SeqCst)).await;
    assert!(!worker.is_finished(), "native failure cannot discard its blocked event prefix");
    assert_eq!(handler.applied_events.load(Ordering::SeqCst), 0);
    handler.event_gate.notify_one();
    wait_for_event("first prefix applied", || handler.applied_events.load(Ordering::SeqCst) == 1).await;
    assert!(!worker.is_finished(), "the queued second prefix must also apply");
    handler.event_gate.notify_one();
    let error = tokio::time::timeout(Duration::from_secs(10), worker).await.unwrap().unwrap().unwrap_err();
    assert_eq!(handler.applied_events.load(Ordering::SeqCst), 2);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
    let detail = error.native_terminal_error.unwrap();
    assert_eq!(detail.get("code"), Some(&"NATIVE_FACTORY_FAILURE".into()));
    assert_eq!(detail.get("message"), Some(&JsValue::String(JsString::from_utf16(
        "failure ".encode_utf16().chain([0xd800]).collect()
    ))));
    assert_eq!(detail.get("fixtureDetail"), Some(&JsValue::Number(7.0)));
}

#[tokio::test]
async fn blocked_native_failure_prefix_reports_bounded_drain_failure() {
    let handler = Handler::new("gated-event");
    let mut limits = limits();
    limits.terminal_drain_timeout = Duration::from_millis(30);
    let client = HostClient::spawn(&mut native_peer(), Some(&"fixture".into()), limits)
        .await.unwrap().with_native_terminal_errors();
    let error = tokio::time::timeout(Duration::from_secs(10), client.execute(request(), handler.clone()))
        .await.unwrap().unwrap_err();
    assert_eq!(error.kind, HostErrorKind::Timeout);
    assert!(error.message.contains("prefix could not be fully applied"));
    assert!(error.native_terminal_error.is_none(), "an incomplete prefix must not be reported as the original failure alone");
    assert_eq!(handler.applied_events.load(Ordering::SeqCst), 0);
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}
