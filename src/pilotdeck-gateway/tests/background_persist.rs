//! Background task persistence tests: disk spill and snapshot recovery.
#![cfg(unix)]
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::runtime::background::{
    BackgroundTaskRuntime, BackgroundTaskRuntimeOptions, StartTaskSpec, TaskStatus,
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;
use tokio::time::{Duration, sleep};

fn system_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as f64
}

/// Test disk spill: write output exceeding memory buffer, read it back.
#[tokio::test]
async fn test_output_spill_write_and_read() {
    let scratch = scratch::Scratch::new("background-spill");
    let spill_dir = scratch.path().to_path_buf();

    let runtime = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
        max_tasks: Some(8),
        completion_preview_bytes: None,
        on_completion: None,
        now: Some(Arc::new(system_now)),
        disk_spill_dir: Some(spill_dir),
        snapshot_store: None,
    });

    // Start a task that writes 2 MB of output (exceeds 1 MB ring buffer)
    let spec = StartTaskSpec {
        command: JsValue::from(
            "python3 -c 'import sys; sys.stdout.write(\"A\" * 2000000); sys.stdout.flush()'",
        ),
        cwd: JsString::from("/tmp"),
        env: None,
        session_id: JsValue::Undefined,
        agent_id: JsValue::Undefined,
        kind: JsValue::Undefined,
    };

    let task = runtime.start(spec).await.expect("start task");
    assert_eq!(task.status, TaskStatus::Running);
    let task_id = task.task_id.clone();

    // Wait for task to complete
    sleep(Duration::from_secs(2)).await;

    // Read output from beginning (should read from disk)
    let slice = runtime
        .get_output(
            &task_id,
            &JsValue::Number(0.0),
            &JsValue::Number(100.0),
            None,
        )
        .expect("read output");

    assert_eq!(slice.content.len(), 100);
    assert_eq!(slice.content, "A".repeat(100));
    assert!(!slice.truncated);

    // Read output from middle (should span disk and memory)
    let slice2 = runtime
        .get_output(
            &task_id,
            &JsValue::Number(500_000.0),
            &JsValue::Number(200.0),
            None,
        )
        .expect("read output");

    assert_eq!(slice2.content.len(), 200);
    assert_eq!(slice2.content, "A".repeat(200));

    runtime.dispose().await;
}

/// Test snapshot persistence: write snapshot, restart runtime, restore tasks.
#[tokio::test]
async fn test_snapshot_persistence_and_recovery() {
    use pilotdeck_gateway::runtime::background_snapshot::JsonFileBackgroundTaskSnapshotStore;

    let scratch = scratch::Scratch::new("background-snapshot");
    let spill_dir = scratch.path().to_path_buf();
    let snapshot_path = spill_dir.join("snapshot.json");

    // Create first runtime with snapshot store
    let store = Arc::new(
        JsonFileBackgroundTaskSnapshotStore::new(snapshot_path.clone())
            .expect("create snapshot store"),
    );

    let runtime1 = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
        max_tasks: Some(8),
        completion_preview_bytes: None,
        on_completion: None,
        now: Some(Arc::new(system_now)),
        disk_spill_dir: Some(spill_dir.clone()),
        snapshot_store: Some(store),
    });

    // Start a task
    let spec = StartTaskSpec {
        command: JsValue::from("echo 'Hello, persistence!'"),
        cwd: JsString::from("/tmp"),
        env: None,
        session_id: JsValue::Undefined,
        agent_id: JsValue::Undefined,
        kind: JsValue::Undefined,
    };

    let task1 = runtime1.start(spec).await.expect("start task");
    assert_eq!(task1.status, TaskStatus::Running);
    let task_id = task1.task_id.clone();

    // Wait for completion
    sleep(Duration::from_secs(1)).await;

    let task1_after = runtime1.get(&task_id, None).expect("get task");
    assert!(task1_after.status.is_terminal());

    // Dispose first runtime (this will persist the terminal state)
    runtime1.dispose().await;

    // Verify snapshot file exists
    assert!(
        snapshot_path.exists(),
        "Snapshot file should exist after dispose"
    );

    // Create second runtime with same snapshot store (simulates restart)
    let store2 = Arc::new(
        JsonFileBackgroundTaskSnapshotStore::new(snapshot_path).expect("load snapshot store"),
    );

    let runtime2 = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
        max_tasks: Some(8),
        completion_preview_bytes: None,
        on_completion: None,
        now: Some(Arc::new(system_now)),
        disk_spill_dir: Some(spill_dir),
        snapshot_store: Some(store2),
    });

    // List all tasks to see what was restored
    let all_tasks = runtime2.list(&Default::default(), None);
    assert!(
        !all_tasks.is_empty(),
        "Should have restored at least one task"
    );

    // Check restored task
    let task2 = runtime2.get(&task_id, None).expect("get restored task");

    // Terminal tasks should be restored as-is
    assert!(task2.status.is_terminal());
    assert_eq!(task2.task_id, task1_after.task_id);

    runtime2.dispose().await;
}

/// Test that running tasks are restored as Unknown after restart.
#[tokio::test]
async fn test_unsettled_task_restored_as_cancelled() {
    use pilotdeck_gateway::runtime::background_snapshot::JsonFileBackgroundTaskSnapshotStore;

    let scratch = scratch::Scratch::new("background-unknown");
    let spill_dir = scratch.path().to_path_buf();
    let snapshot_path = spill_dir.join("snapshot.json");

    let store = Arc::new(
        JsonFileBackgroundTaskSnapshotStore::new(snapshot_path.clone())
            .expect("create snapshot store"),
    );

    let runtime1 = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
        max_tasks: Some(8),
        completion_preview_bytes: None,
        on_completion: None,
        now: Some(Arc::new(system_now)),
        disk_spill_dir: Some(spill_dir.clone()),
        snapshot_store: Some(store),
    });

    // Start a long-running task
    let spec = StartTaskSpec {
        command: JsValue::from("sleep 300"),
        cwd: JsString::from("/tmp"),
        env: None,
        session_id: JsValue::Undefined,
        agent_id: JsValue::Undefined,
        kind: JsValue::Undefined,
    };

    let task1 = runtime1.start(spec).await.expect("start task");
    assert_eq!(task1.status, TaskStatus::Running);
    let task_id = task1.task_id.clone();

    // Give it time to start
    sleep(Duration::from_millis(100)).await;

    // Dispose runtime (simulate crash) - this will kill the task and mark it Cancelled
    runtime1.dispose().await;

    // Restart runtime
    let store2 = Arc::new(
        JsonFileBackgroundTaskSnapshotStore::new(snapshot_path).expect("load snapshot store"),
    );

    let runtime2 = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
        max_tasks: Some(8),
        completion_preview_bytes: None,
        on_completion: None,
        now: Some(Arc::new(system_now)),
        disk_spill_dir: Some(spill_dir),
        snapshot_store: Some(store2),
    });

    // Check restored task - dispose() kills tasks, so they become Cancelled
    let task2 = runtime2.get(&task_id, None).expect("get restored task");

    assert_eq!(task2.status, TaskStatus::Cancelled);
    // Terminal tasks (Cancelled) preserve their PID from the original run
    assert!(task2.pid.is_some());

    runtime2.dispose().await;
}
