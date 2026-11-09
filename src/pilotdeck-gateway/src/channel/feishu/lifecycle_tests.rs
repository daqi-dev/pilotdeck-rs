//! Owner accounting and same-instance restart; actual A7 stop coverage is in actual_host_tests.
use super::*;
fn deps() -> ChannelStartDeps {
    ChannelStartDeps {
        config: None,
        report_status: None,
        log: None,
        turn_gateway: None,
    }
}
fn channel() -> FeishuChannel {
    FeishuChannel::new(
        FeishuOptions {
            connection_mode: "webhook".into(),
            base_url: "http://127.0.0.1:1".into(),
            ..Default::default()
        },
        Arc::new(NoInteraction),
    )
}
#[tokio::test]
async fn stop_drains_answer_reservation_and_same_channel_can_restart() {
    let channel = channel();
    let handle = channel.runtime.start(deps()).await.unwrap();
    let runtime = channel.runtime.clone();
    let (work, stop) = runtime.begin_work().unwrap();
    {
        let mut state = runtime.state.lock().unwrap();
        state.permission_answering.insert("chat".into(), 7);
        state.chats.entry("chat".into()).or_default().active_run = Some(ActiveRun {
            session_key: "feishu:chat=chat:general".into(),
            run_id: None,
            generation: 0,
        });
    }
    let answering = AnsweringSlot {
        runtime: runtime.clone(),
        chat: "chat".into(),
        token: 7,
        _work: Some(work),
    };
    let task = tokio::spawn(async move {
        let _answering = answering;
        stop.cancelled().await;
    });
    tokio::time::timeout(Duration::from_secs(2), handle.stop("test-stop"))
        .await
        .unwrap()
        .unwrap();
    task.await.unwrap();
    assert_eq!(
        runtime.owned_work.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    {
        let state = runtime.state.lock().unwrap();
        assert!(state.permission_answering.is_empty());
        assert!(state.chats["chat"].active_run.is_none());
        assert!(state.chats["chat"].queue.is_empty());
    }
    assert!(
        runtime.begin_work().is_none(),
        "stopped adapter must reject new owned work"
    );
    let next = runtime.start(deps()).await.unwrap();
    let (work, signal) = runtime.begin_work().expect("same instance restarts");
    assert!(!signal.is_aborted());
    drop(work);
    next.stop("restarted-stop").await.unwrap();
}
#[test]
fn cancelling_unpolled_answer_future_releases_token_before_work_lease() {
    let channel = channel();
    let runtime = channel.runtime.clone();
    let (work, _) = runtime.begin_work().unwrap();
    runtime
        .state
        .lock()
        .unwrap()
        .permission_answering
        .insert("chat".into(), 11);
    let slot = AnsweringSlot {
        runtime: runtime.clone(),
        chat: "chat".into(),
        token: 11,
        _work: Some(work),
    };
    let future = async move {
        let _slot = slot;
        std::future::pending::<()>().await;
    };
    drop(future);
    assert!(
        runtime
            .state
            .lock()
            .unwrap()
            .permission_answering
            .is_empty()
    );
    assert_eq!(
        runtime.owned_work.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}
