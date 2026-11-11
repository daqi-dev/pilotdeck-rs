use super::*;
#[test]
fn default_api_origin_has_exactly_one_api_prefix() {
    for path in [
        "/api/apps.connections.open",
        "/api/auth.test",
        "/api/chat.postMessage",
    ] {
        assert_eq!(
            endpoint_url(&DEFAULT_BASE_URL.into(), path),
            format!("https://slack.com{path}")
        );
        assert!(!endpoint_url(&DEFAULT_BASE_URL.into(), path).contains("/api/api/"));
    }
}

fn cleanup_fixture(generation: u64) -> Arc<SlackShared> {
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
async fn auth_failure_aborts_the_reader_before_waiting_for_its_runtime_lock() {
    let shared = cleanup_fixture(7);
    shared.runtime.lock().await.app = Some(LiveApp { sink: None });
    let (parked, entered) = tokio::sync::oneshot::channel();
    let owner = shared.clone();
    let reader = tokio::spawn(async move {
        let _runtime = owner.runtime.lock().await;
        parked.send(()).unwrap();
        // Deterministic substitute for an ACK's pending sink send. No socket
        // buffer pressure or timing-dependent network behavior is required.
        std::future::pending::<()>().await;
    });
    entered.await.unwrap();
    let abort = reader.abort_handle();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        abandon_session(&shared, 7, &abort),
    )
    .await;
    // Always retire the controlled parked task, including on a regression.
    reader.abort();
    assert!(reader.await.unwrap_err().is_cancelled());
    assert!(
        result.is_ok(),
        "auth failure queued behind its parked reader"
    );
    assert!(shared.runtime.lock().await.app.is_none());
}

#[tokio::test]
async fn stale_auth_failure_aborts_only_its_reader_and_keeps_the_new_app() {
    let shared = cleanup_fixture(8);
    shared.runtime.lock().await.app = Some(LiveApp { sink: None });
    let old = tokio::spawn(std::future::pending::<()>());
    let new = tokio::spawn(std::future::pending::<()>());
    let old_abort = old.abort_handle();
    abandon_session(&shared, 7, &old_abort).await;
    let new_was_running = !new.is_finished();
    let app_was_kept = shared.runtime.lock().await.app.is_some();
    old.abort();
    new.abort();
    assert!(old.await.unwrap_err().is_cancelled());
    assert!(new.await.unwrap_err().is_cancelled());
    assert!(new_was_running, "old auth failure aborted the new reader");
    assert!(app_was_kept, "old auth failure removed the new app");
}
