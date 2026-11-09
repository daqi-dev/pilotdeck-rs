use super::*;

#[tokio::test]
async fn old_permission_callback_never_targets_or_removes_a_new_turn_sink() {
    let coordinator = Arc::new(TurnEventCoordinator::new());
    let run = |id: &str| RunRef {
        session_key: "same-session".into(),
        run_id: id.into(),
        project_key: None,
    };
    let old = Arc::new(TurnPublisher::start(run("old"), None, coordinator.clone()));
    let old_emit = old.permission_emitter();
    let new = Arc::new(TurnPublisher::start(run("new"), None, coordinator));
    let new_emit = new.permission_emitter();
    let event = crate::metadata::object([("type", "permission_request".into())]);
    assert!(
        !old_emit(event.clone()),
        "old callback was retargeted to the new turn"
    );
    assert!(
        new_emit(event.clone()),
        "stale callback removed the new registration"
    );
    let delivered = tokio::time::timeout(std::time::Duration::from_secs(1), new.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.get("runId"), Some(&"new".into()));
    new.finish().await;
    assert!(
        !new_emit(event.clone()),
        "permission event crossed finish fence"
    );
    drop(new);
    assert!(
        !new_emit(event),
        "weak callback retained a dropped publisher"
    );
}
