use super::*;

fn native(registry: Arc<ControlledRegistry>) -> TurnPublisher {
    registry.file.accept(&input()).unwrap();
    TurnPublisher::start_reliable(
        run_ref(),
        Some(registry),
        Arc::new(TurnEventCoordinator::default()),
        Duration::from_secs(1),
    )
}
fn timeout_event() -> JsValue {
    js(&json!({"type":"error","code":"turn_timeout","runId":"r"}))
}

#[tokio::test]
async fn settled_writes_remain_hidden_until_real_close_decision_allows() {
    let registry = ControlledRegistry::new(&json!({}));
    let publisher = native(registry.clone());
    let gate = publisher
        .begin_causal_close(tokio::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    publisher.gateway_event(timeout_event());
    registry.release(0, false).await;
    // The append really settled before request_close: it must still not escape.
    assert!(
        tokio::time::timeout(Duration::from_millis(10), publisher.next())
            .await
            .is_err()
    );
    gate.request_close();
    assert!(
        tokio::time::timeout(Duration::from_millis(10), publisher.next())
            .await
            .is_err()
    );
    gate.allow();
    assert_eq!(publisher.next().await, Some(timeout_event()));
    assert_eq!(publisher.next().await, None);
    publisher.finish().await;
}

#[tokio::test]
async fn pending_or_cancelled_decision_preserves_queued_prefix_but_drops_timeout() {
    let registry = ControlledRegistry::new(&json!({}));
    let publisher = native(registry.clone());
    let prefix = js(&json!({"type":"model_request_started","runId":"r"}));
    publisher.gateway_event(prefix.clone());
    registry.release(0, false).await;
    while !lock(&publisher.publication.state).pending.is_empty() {
        tokio::task::yield_now().await;
    }
    let gate = publisher
        .begin_causal_close(tokio::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    publisher.gateway_event(timeout_event());
    registry.release(1, false).await;
    gate.request_close();
    drop(gate); // cancellation/panic and a pending pump all fail closed
    assert_eq!(publisher.next().await, Some(prefix));
    assert_eq!(publisher.next().await, None);
    publisher.finish().await;
    assert_eq!(
        registry.file.events(&run_ref(), None, None).unwrap().len(),
        2,
        "denying live delivery does not undo real accepted disk IO"
    );
}

#[tokio::test(start_paused = true)]
async fn decision_and_durability_share_one_absolute_deadline() {
    let registry = ControlledRegistry::new(&json!({}));
    let publisher = native(registry.clone());
    let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
    let gate = publisher.begin_causal_close(deadline).unwrap();
    publisher.gateway_event(timeout_event());
    gate.request_close();
    tokio::time::advance(Duration::from_millis(80)).await;
    gate.allow(); // twenty milliseconds remain, not a fresh reliability budget
    let visible = publisher.next().await.unwrap();
    assert_eq!(field(&visible, "code"), &JsValue::from("result_unknown"));
    assert_eq!(tokio::time::Instant::now(), deadline);
    assert_eq!(publisher.next().await, None);
    registry.release(0, false).await;
    publisher.finish().await;
    assert_eq!(
        publisher.next().await,
        None,
        "late durability cannot reopen delivery"
    );
}

#[tokio::test(start_paused = true)]
async fn late_pump_exit_cannot_authorize_an_expired_gate() {
    let registry = ControlledRegistry::new(&json!({}));
    let publisher = native(registry.clone());
    let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
    let gate = publisher.begin_causal_close(deadline).unwrap();
    publisher.gateway_event(timeout_event());
    registry.release(0, false).await;
    gate.request_close();
    tokio::time::advance(Duration::from_millis(100)).await;
    gate.allow();
    assert_eq!(publisher.next().await, None);
    publisher.finish().await;
}

#[tokio::test]
async fn gate_drop_wakes_a_waiting_consumer_without_releasing_held_events() {
    let registry = ControlledRegistry::new(&json!({}));
    let publisher = Arc::new(native(registry.clone()));
    let gate = publisher
        .begin_causal_close(tokio::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    publisher.gateway_event(timeout_event());
    registry.release(0, false).await;
    gate.request_close();
    let reader = publisher.clone();
    let next = tokio::spawn(async move { reader.next().await });
    tokio::task::yield_now().await;
    assert!(!next.is_finished());
    drop(gate);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), next)
            .await
            .unwrap()
            .unwrap(),
        None
    );
    publisher.finish().await;
}

#[tokio::test]
async fn no_registry_keeps_synchronous_delivery_without_a_gate() {
    let publisher = TurnPublisher::start_reliable(
        run_ref(),
        None,
        Arc::new(TurnEventCoordinator::default()),
        Duration::from_secs(1),
    );
    assert!(
        publisher
            .begin_causal_close(tokio::time::Instant::now() + Duration::from_secs(1))
            .is_none()
    );
    publisher.gateway_event(timeout_event());
    publisher.close_live();
    assert_eq!(publisher.next().await, Some(timeout_event()));
    assert_eq!(publisher.next().await, None);
    publisher.finish().await;
}
