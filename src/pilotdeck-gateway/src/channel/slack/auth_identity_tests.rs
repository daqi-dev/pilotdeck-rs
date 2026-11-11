use super::*;

fn fixture(generation: u64) -> Arc<SlackShared> {
    let channel = SlackChannel::new(SlackChannelOptions {
        bot_token: Some("mock-bot".into()),
        app_token: Some("mock-app".into()),
        mapper: None,
        gateway: None,
        env: None,
        base_url: None,
    });
    channel
        .shared
        .generation
        .store(generation, Ordering::SeqCst);
    channel.shared
}

#[tokio::test]
async fn successful_auth_records_identity_without_waiting_for_a_parked_ack() {
    let shared = fixture(7);
    let (started, entered) = tokio::sync::oneshot::channel();
    let owner = shared.clone();
    let reader = tokio::spawn(async move {
        let _runtime = owner.runtime.lock().await;
        started.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    entered.await.unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        record_authenticated_bot(&shared, 7, JsValue::from("BOT")),
    )
    .await;
    let identity = shared
        .bot_user_id
        .lock()
        .expect("Slack bot identity mutex")
        .clone();
    let reader_was_running = !reader.is_finished();
    reader.abort();
    let _ = reader.await;
    assert!(
        matches!(result, Ok(Ok(()))),
        "successful auth queued behind ACK"
    );
    assert_eq!(identity, JsValue::from("BOT"));
    assert!(reader_was_running, "successful auth aborted its ACK reader");
}

#[tokio::test]
async fn stale_successful_auth_cannot_replace_the_new_generation_identity() {
    let shared = fixture(8);
    *shared.bot_user_id.lock().expect("Slack bot identity mutex") = JsValue::from("NEW");
    assert!(
        record_authenticated_bot(&shared, 7, JsValue::from("OLD"))
            .await
            .is_err()
    );
    assert_eq!(
        *shared.bot_user_id.lock().expect("Slack bot identity mutex"),
        JsValue::from("NEW")
    );
}
