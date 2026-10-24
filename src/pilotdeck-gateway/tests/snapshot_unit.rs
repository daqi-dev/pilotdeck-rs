//! Simple unit test for snapshot store
#![cfg(unix)]

use pilotdeck_gateway::runtime::background_snapshot::{
    BackgroundTaskSnapshotStore, JsonFileBackgroundTaskSnapshotStore, PersistedBackgroundTask,
    PersistedOutput, PersistedTask,
};
use std::sync::Arc;

#[tokio::test]
async fn test_snapshot_store_write_and_load() {
    let temp_dir = std::env::temp_dir();
    let snapshot_path = temp_dir.join(format!("test-snapshot-{}.json", uuid::Uuid::new_v4()));

    // Create store (file doesn't exist yet)
    let store = Arc::new(
        JsonFileBackgroundTaskSnapshotStore::new(snapshot_path.clone())
            .expect("create snapshot store"),
    );

    // Write a task
    let task = PersistedBackgroundTask {
        task: PersistedTask {
            task_id: "test-task-1".to_string(),
            task_type: "local_bash".to_string(),
            kind: "bash".to_string(),
            command: "echo hello".to_string(),
            cwd: "/tmp".to_string(),
            status: "completed".to_string(),
            completion_status_sent_in_attachment: false,
            last_reported_total_lines: 0,
            is_backgrounded: true,
            interrupted: false,
            started_at: "2024-01-01T00:00:00.000Z".to_string(),
            ended_at: Some("2024-01-01T00:00:01.000Z".to_string()),
            agent_id: None,
            session_id: None,
            pid: Some(12345),
            exit_code: Some(Some(0)),
            output_bytes: 100,
        },
        output: PersistedOutput {
            total_bytes: 100,
            persisted: true,
        },
    };

    store.write(task.clone()).await.expect("write task");
    store.flush().await.expect("flush");

    // Verify file exists
    assert!(
        snapshot_path.exists(),
        "Snapshot file should exist: {:?}",
        snapshot_path
    );

    // Load in new store instance
    let store2 = Arc::new(
        JsonFileBackgroundTaskSnapshotStore::new(snapshot_path.clone())
            .expect("load snapshot store"),
    );

    let loaded = store2.load();
    assert_eq!(loaded.len(), 1, "Should have loaded 1 task");
    assert_eq!(loaded[0].task.task_id, "test-task-1");
    assert_eq!(loaded[0].task.status, "completed");

    // Cleanup
    let _ = std::fs::remove_file(snapshot_path);
}
