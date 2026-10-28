//! Background task persistence tests: disk spill and snapshot recovery.
#![cfg(unix)]
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::runtime::background::{
    BackgroundTaskRuntime, BackgroundTaskRuntimeOptions, StartTaskSpec, TaskStatus,
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;
use tokio::time::{Duration, timeout};

fn system_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("#978 system clock is after the Unix epoch")
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
        disk_spill_dir: Some(spill_dir.clone()),
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

    let task = runtime
        .start(spec)
        .await
        .expect("#978 start background task");
    assert_eq!(
        task.status,
        TaskStatus::Running,
        "#978 spill starts running"
    );
    let task_id = task.task_id.clone();

    // Completion is an observable event, not an estimate of Python startup.
    let completed = timeout(
        Duration::from_secs(60),
        runtime.wait(&task_id, &JsValue::Undefined, None, None),
    )
    .await
    .expect("#978 spill completion deadline")
    .expect("#978 wait for spill task")
    .expect("#978 spill task remains registered");
    assert_eq!(completed.outcome, "completed", "#978 spill wait outcome");
    assert_eq!(
        completed.task.status,
        TaskStatus::Completed,
        "#978 spill producer completed successfully"
    );
    assert_eq!(
        completed.task.output_bytes, 2_000_000,
        "#978 spill producer delivered all bytes"
    );

    // done does not flush the asynchronous spill chain. Keep the original
    // read-before-dispose boundary unchanged: observe actual disk progress
    // before checking the public slice API, without closing the output store.
    let output_path = spill_dir.join("output").join(format!(
        "{}.log",
        task_id.to_utf8().expect("#978 spill task id is UTF-8")
    ));
    let mut persisted = 0;
    timeout(Duration::from_secs(60), async {
        loop {
            persisted = match std::fs::metadata(&output_path) {
                Ok(metadata) => metadata.len(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                Err(error) => panic!("#978 inspect spill progress: {error}"),
            };
            if persisted >= 2_000_000 {
                break;
            }
            // Polling cadence only; no amount of elapsed time implies success.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("#978 spill write deadline: persisted={persisted}/2000000"));
    assert_eq!(persisted, 2_000_000, "#978 exact spill file length");

    // Read output from beginning (should read from disk)
    let slice = runtime
        .get_output(
            &task_id,
            &JsValue::Number(0.0),
            &JsValue::Number(100.0),
            None,
        )
        .expect("#978 read spill output");

    assert_eq!(slice.content.len(), 100, "#978 spill prefix length");
    assert_eq!(slice.content, "A".repeat(100), "#978 spill prefix bytes");
    assert!(!slice.truncated, "#978 spill prefix is not truncated");

    // Read an earlier offset that has also left the memory ring.
    let slice2 = runtime
        .get_output(
            &task_id,
            &JsValue::Number(500_000.0),
            &JsValue::Number(200.0),
            None,
        )
        .expect("#978 read spill output");

    assert_eq!(slice2.content.len(), 200, "#978 spill middle length");
    assert_eq!(slice2.content, "A".repeat(200), "#978 spill middle bytes");

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
            .expect("#978 create snapshot store"),
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

    let task1 = runtime1
        .start(spec)
        .await
        .expect("#978 start background task");
    assert_eq!(
        task1.status,
        TaskStatus::Running,
        "#978 snapshot starts running"
    );
    let task_id = task1.task_id.clone();

    let completed = timeout(
        Duration::from_secs(60),
        runtime1.wait(&task_id, &JsValue::Undefined, None, None),
    )
    .await
    .expect("#978 snapshot completion deadline")
    .expect("#978 wait for snapshot task")
    .expect("#978 snapshot task remains registered");
    assert_eq!(completed.outcome, "completed", "#978 snapshot wait outcome");
    let task1_after = runtime1
        .get(&task_id, None)
        .expect("#978 get completed task");
    assert_eq!(
        task1_after.status,
        TaskStatus::Completed,
        "#978 snapshot producer completed successfully"
    );

    // Dispose first runtime (this will persist the terminal state)
    runtime1.dispose().await;

    // Verify snapshot file exists
    assert!(
        snapshot_path.exists(),
        "#978 snapshot file exists after dispose"
    );

    // Create second runtime with same snapshot store (simulates restart)
    let store2 = Arc::new(
        JsonFileBackgroundTaskSnapshotStore::new(snapshot_path).expect("#978 load snapshot store"),
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
        "#978 snapshot restores at least one task"
    );

    // Check restored task
    let task2 = runtime2
        .get(&task_id, None)
        .expect("#978 get restored task");

    // Terminal tasks should be restored as-is
    assert_eq!(
        task2.status, task1_after.status,
        "#978 restored completed status"
    );
    assert_eq!(
        task2.task_id, task1_after.task_id,
        "#978 restored task identity"
    );

    runtime2.dispose().await;
}

/// Disposing a running task persists Cancelled, including its original PID.
#[tokio::test]
async fn test_unsettled_task_restored_as_cancelled() {
    use pilotdeck_gateway::runtime::background_snapshot::JsonFileBackgroundTaskSnapshotStore;

    let scratch = scratch::Scratch::new("background-unknown");
    let spill_dir = scratch.path().to_path_buf();
    let snapshot_path = spill_dir.join("snapshot.json");

    let store = Arc::new(
        JsonFileBackgroundTaskSnapshotStore::new(snapshot_path.clone())
            .expect("#978 create snapshot store"),
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

    let task1 = runtime1
        .start(spec)
        .await
        .expect("#978 start background task");
    assert_eq!(
        task1.status,
        TaskStatus::Running,
        "#978 snapshot starts running"
    );
    let task_id = task1.task_id.clone();

    // start has already acknowledged the spawn. There is no later shell
    // milestone needed before cancellation; do not guess one with a sleep.
    // Dispose deliberately, killing the task and persisting Cancelled.
    runtime1.dispose().await;
    // Check the immutable creation snapshot after cleanup, so a missing-PID
    // negative control cannot strand the detached sleep process.
    assert!(task1.pid.is_some(), "#978 start acknowledged the child PID");

    // Restart runtime
    let store2 = Arc::new(
        JsonFileBackgroundTaskSnapshotStore::new(snapshot_path).expect("#978 load snapshot store"),
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
    let task2 = runtime2
        .get(&task_id, None)
        .expect("#978 get restored task");

    assert_eq!(
        task2.status,
        TaskStatus::Cancelled,
        "#978 restored cancelled status"
    );
    // Terminal tasks (Cancelled) preserve their PID from the original run
    assert!(task2.pid.is_some(), "#978 restored cancelled PID");

    runtime2.dispose().await;
}
