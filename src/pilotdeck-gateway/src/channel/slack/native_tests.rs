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
    let mut old = tokio::spawn(std::future::pending::<()>());
    let (started, entered) = tokio::sync::oneshot::channel();
    let new = tokio::spawn(async move {
        started.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    entered.await.unwrap();
    let new_abort = new.abort_handle();
    shared.readers.lock().expect("Slack reader mutex").push(new);
    let old_abort = old.abort_handle();
    let cleanup = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        abandon_session(&shared, 7, &old_abort),
    )
    .await;
    // Await the cancellation produced by the tested helper itself. Capture all
    // observations before test cleanup can abort either controlled task.
    let old_join = tokio::time::timeout(std::time::Duration::from_secs(1), &mut old).await;
    let old_was_aborted = matches!(&old_join, Ok(Err(error)) if error.is_cancelled());
    let new_was_running = !new_abort.is_finished();
    let registry_kept_new = {
        let readers = shared.readers.lock().expect("Slack reader mutex");
        readers.len() == 1 && readers[0].id() == new_abort.id()
    };
    let app_was_kept = shared.runtime.lock().await.app.is_some();
    if old_join.is_err() {
        old.abort();
        let _ = old.await;
    }
    let readers = std::mem::take(&mut *shared.readers.lock().expect("Slack reader mutex"));
    new_abort.abort();
    for reader in readers {
        reader.abort();
        let _ = reader.await;
    }
    assert!(cleanup.is_ok(), "stale auth cleanup did not complete");
    assert!(
        old_was_aborted,
        "stale auth cleanup did not abort its own reader"
    );
    assert!(new_was_running, "old auth failure aborted the new reader");
    assert!(
        registry_kept_new,
        "old auth failure removed the new reader registry entry"
    );
    assert!(app_was_kept, "old auth failure removed the new app");
}
