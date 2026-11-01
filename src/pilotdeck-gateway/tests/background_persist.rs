//! Background task persistence tests: disk spill and snapshot recovery.
#![cfg(unix)]
#[allow(dead_code)]
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::runtime::background::{
    BackgroundBashTask, BackgroundResources, BackgroundSubagentRuntime, BackgroundTaskError,
    BackgroundTaskRuntime, BackgroundTaskRuntimeOptions, CompletionHandler, StartTaskSpec,
    TaskStatus,
};
use pilotdeck_gateway::runtime::background_snapshot::{
    BackgroundTaskSnapshotStore, JsonFileBackgroundTaskSnapshotStore, PersistedBackgroundTask,
    PersistedOutput,
};
use pilotdeck_gateway::runtime::project::RuntimeResources;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use serde_json::Value;
use std::{
    future::Future,
    io,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
};
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

    runtime.dispose().await.expect("#978 spill dispose");
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
    runtime1.dispose().await.expect("#978 first dispose");

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

    runtime2.dispose().await.expect("#978 restored dispose");
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
    runtime1.dispose().await.expect("#978 unsettled dispose");
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

    runtime2
        .dispose()
        .await
        .expect("#978 cancelled restored dispose");
}

// ===================== #1233: dispose flush errors and restart recovery =====================

const PERSIST_ORACLE: &str = include_str!("fixtures/background-persist-restart/oracle.json");

/// The `background-persist-restart` oracle case with the given id. The
/// optional-baseline git verification is bounded and runs once per process
/// (see `baseline::oracle_bounded`).
fn persist_case(id: &str) -> Value {
    static PARSED: OnceLock<Value> = OnceLock::new();
    let parsed = PARSED.get_or_init(|| {
        let _ = baseline::oracle_bounded(PERSIST_ORACLE);
        serde_json::from_str(PERSIST_ORACLE).expect("#1233 oracle JSON")
    });
    parsed["cases"]
        .as_array()
        .expect("#1233 oracle cases array")
        .iter()
        .find(|case| case["id"] == id)
        .unwrap_or_else(|| panic!("#1233 oracle case {id}"))
        .clone()
}

/// The raw on-disk snapshot file, as a crash would leave it.
fn read_snapshot_file(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("#1233 read snapshot file"))
        .expect("#1233 snapshot JSON")
}

fn task_id_string(task: &BackgroundBashTask) -> String {
    String::from_utf16_lossy(task.task_id.units())
}

async fn start_command(
    runtime: &BackgroundTaskRuntime,
    command: &str,
    cwd: &Path,
) -> Result<BackgroundBashTask, BackgroundTaskError> {
    runtime
        .start(StartTaskSpec {
            command: JsValue::from(command),
            cwd: JsString::from(cwd.to_str().expect("#1233 cwd is UTF-8")),
            env: None,
            session_id: JsValue::Undefined,
            agent_id: JsValue::Undefined,
            kind: JsValue::Undefined,
        })
        .await
}

async fn start_and_wait(
    runtime: &BackgroundTaskRuntime,
    command: &str,
    cwd: &Path,
) -> BackgroundBashTask {
    let task = start_command(runtime, command, cwd)
        .await
        .expect("#1233 start background task");
    let waited = timeout(
        Duration::from_secs(30),
        runtime.wait(&task.task_id, &JsValue::Undefined, None, None),
    )
    .await
    .expect("#1233 completion deadline")
    .expect("#1233 task stays registered")
    .expect("#1233 wait for background task");
    assert_eq!(waited.outcome, "completed", "#1233 {command} completes");
    task
}

fn completion_sink() -> (CompletionHandler, Arc<Mutex<Vec<String>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let events = Arc::clone(&events);
        Arc::new(move |event: JsObject| {
            let value = JsValue::Object(event);
            let status = value
                .get("status")
                .and_then(|status| status.as_string())
                .map(|status| String::from_utf16_lossy(status.units()))
                .unwrap_or_default();
            events.lock().unwrap().push(status);
        })
    };
    (sink, events)
}

fn configured_runtime(
    state: &Path,
    store: Option<Arc<dyn BackgroundTaskSnapshotStore>>,
    events: Option<CompletionHandler>,
) -> BackgroundTaskRuntime {
    let store = store.unwrap_or_else(|| {
        Arc::new(
            JsonFileBackgroundTaskSnapshotStore::new(state.join("state.json"))
                .expect("#1233 create snapshot store"),
        )
    });
    BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
        disk_spill_dir: Some(state.to_path_buf()),
        snapshot_store: Some(store),
        on_completion: events,
        ..Default::default()
    })
}

fn message(error: &BackgroundTaskError) -> String {
    String::from_utf16_lossy(error.message.units())
}

/// A snapshot store whose `flush` refuses like the baseline's dispose-flush
/// probes: either the returned future rejects, or `flush` throws before
/// returning a promise at all.
struct FlushFailureStore {
    inner: Arc<JsonFileBackgroundTaskSnapshotStore>,
    sync: bool,
}

impl BackgroundTaskSnapshotStore for FlushFailureStore {
    fn load(&self) -> Vec<PersistedBackgroundTask> {
        self.inner.load()
    }
    fn write(
        &self,
        snapshot: PersistedBackgroundTask,
        seq: u64,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        self.inner.write(snapshot, seq)
    }
    fn flush(&self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        if self.sync {
            std::panic::panic_any("#1233 flush refused");
        }
        Box::pin(async { Err(io::Error::other("#1233 flush refused")) })
    }
}

/// A6.5-BACKGROUND-PANIC-001-R2 (D1): a disposing flush failure rejects every
/// `dispose()` caller with the same error, keeps the runtime draining, refuses
/// new starts and keeps reads alive. The rejection is memoized.
#[tokio::test]
async fn dispose_flush_failure_stays_draining_and_memoizes_the_rejection() {
    for sync in [false, true] {
        let mode = if sync { "sync-throw" } else { "reject" };
        let case = persist_case(&format!("dispose-flush-{mode}"));
        let scratch = scratch::Scratch::new("1233-flush-failure");
        let state = scratch.path().join("state");
        let (sink, events) = completion_sink();
        let runtime = configured_runtime(
            &state,
            Some(Arc::new(FlushFailureStore {
                inner: Arc::new(
                    JsonFileBackgroundTaskSnapshotStore::new(state.join("state.json"))
                        .expect("#1233 create snapshot store"),
                ),
                sync,
            })),
            Some(sink),
        );
        let task = start_and_wait(&runtime, "printf abcdef", scratch.path()).await;

        let failed = runtime
            .dispose()
            .await
            .expect_err("#1233 flush failure rejects dispose");
        assert_eq!(
            message(&failed),
            "#1233 flush refused",
            "#1233 {mode} flush message"
        );
        assert_eq!(
            format!("rejected: {}", message(&failed)),
            case["outcome"].as_str().expect("#1233 oracle outcome"),
            "#1233 {mode} dispose outcome matches the baseline"
        );

        let refused = start_command(&runtime, "true", scratch.path())
            .await
            .expect_err("#1233 draining runtime refuses a new task");
        assert_eq!(
            message(&refused),
            case["startError"]
                .as_str()
                .expect("#1233 oracle startError"),
            "#1233 {mode} start refusal"
        );
        assert!(
            message(&refused)
                .starts_with("Background task runtime is draining; refusing a new task."),
            "#1233 {mode} runtime stays draining"
        );

        let listed: Vec<(String, Option<i32>)> = runtime
            .list(&Default::default(), None)
            .iter()
            .map(|task| (task.status.as_str().to_owned(), task.exit_code.flatten()))
            .collect();
        let expected: Vec<(String, Option<i32>)> = case["listed"]
            .as_array()
            .expect("#1233 oracle listed")
            .iter()
            .map(|entry| {
                (
                    entry["status"].as_str().expect("#1233 status").to_owned(),
                    entry["exitCode"].as_i64().map(|code| code as i32),
                )
            })
            .collect();
        assert_eq!(listed, expected, "#1233 {mode} list keeps terminal tasks");

        let read = runtime
            .get_output(
                &task.task_id,
                &JsValue::Number(0.0),
                &JsValue::Undefined,
                None,
            )
            .expect("#1233 get_output works while draining");
        assert_eq!(
            (
                read.content.as_str(),
                read.next_offset,
                read.total_bytes,
                read.truncated
            ),
            (
                case["readAfterDispose"]["content"]
                    .as_str()
                    .expect("#1233 oracle content"),
                case["readAfterDispose"]["nextOffset"]
                    .as_f64()
                    .expect("#1233 oracle offset"),
                case["readAfterDispose"]["totalBytes"]
                    .as_f64()
                    .expect("#1233 oracle bytes"),
                case["readAfterDispose"]["truncated"]
                    .as_bool()
                    .expect("#1233 oracle truncated"),
            ),
            "#1233 {mode} read after dispose"
        );
        assert_eq!(
            *events.lock().unwrap(),
            case["completionEvents"]
                .as_array()
                .expect("#1233 oracle events")
                .iter()
                .map(|status| status.as_str().expect("#1233 event status").to_owned())
                .collect::<Vec<_>>(),
            "#1233 {mode} completion events"
        );

        let again = runtime
            .dispose()
            .await
            .expect_err("#1233 later dispose reuses the rejection");
        assert!(
            Arc::ptr_eq(&failed, &again),
            "#1233 {mode} the rejection is memoized"
        );
        assert_eq!(message(&again), "#1233 flush refused", "#1233 {mode} reuse");
    }
}

/// A6.5-BACKGROUND-PERSIST-001-R2 (D2/D3): terminal tasks survive a dispose and
/// restart with their output readable from the spill log.
#[tokio::test]
async fn terminal_recovery_restart_restores_completed_tasks_with_output() {
    let case = persist_case("terminal-recovery");
    let scratch = scratch::Scratch::new("1233-terminal-recovery");
    let state = scratch.path().join("state");
    let (sink, events) = completion_sink();
    let runtime = configured_runtime(&state, None, Some(sink));
    let printf = start_and_wait(&runtime, "printf abcdef", scratch.path()).await;
    let silent = start_and_wait(&runtime, "true", scratch.path()).await;
    runtime.dispose().await.expect("#1233 terminal dispose");

    let mut on_disk = read_snapshot_file(&state.join("state.json"));
    let tasks = on_disk["tasks"]
        .as_array_mut()
        .expect("#1233 snapshot tasks");
    assert_eq!(tasks.len(), 2, "#1233 both tasks snapshotted");
    tasks.sort_by(|a, b| {
        a["task"]["command"]
            .as_str()
            .expect("#1233 command")
            .cmp(b["task"]["command"].as_str().expect("#1233 command"))
    });
    let expected = case["snapshot"].as_array().expect("#1233 oracle snapshot");
    for (entry, oracle) in tasks.iter().zip(expected) {
        assert_eq!(
            entry["task"]["status"], oracle["status"],
            "#1233 snapshot status"
        );
        assert_eq!(
            entry["task"]["exitCode"], oracle["exitCode"],
            "#1233 snapshot exit code"
        );
        assert!(
            entry["task"]["pid"].is_number(),
            "#1233 snapshot keeps the pid"
        );
        assert_eq!(
            entry["task"]["interrupted"], oracle["interrupted"],
            "#1233 snapshot interrupted"
        );
        assert!(
            entry["task"]["startedAt"].is_string() && entry["task"]["endedAt"].is_string(),
            "#1233 snapshot timestamps"
        );
        assert_eq!(
            entry["task"]["outputBytes"], oracle["outputBytes"],
            "#1233 snapshot output bytes"
        );
        assert_eq!(
            entry["output"]["totalBytes"], oracle["output"]["totalBytes"],
            "#1233 snapshot durable bytes"
        );
        // A6.5-BACKGROUND-PERSIST-001-R2 (D6, registered deviation): the
        // baseline's `hasDurableOutput` is `diskSpillPath !== null`, so an
        // empty log is claimed `persisted:true`. Rust only claims bytes that
        // actually landed, so an empty log keeps `persisted:false`; a restart
        // restores it either way (ENOENT with zero expected bytes).
        assert_eq!(
            entry["output"]["persisted"],
            entry["output"]["totalBytes"]
                .as_u64()
                .expect("#1233 total bytes")
                > 0,
            "#1233 snapshot persists only bytes that landed"
        );
    }

    let (second_sink, second_events) = completion_sink();
    let runtime2 = configured_runtime(&state, None, Some(second_sink));
    let listed = runtime2.list(&Default::default(), None);
    assert_eq!(listed.len(), 2, "#1233 restart restores both tasks");
    let listed_ids: std::collections::BTreeSet<String> =
        listed.iter().map(task_id_string).collect();
    assert_eq!(
        listed_ids,
        std::collections::BTreeSet::from([task_id_string(&printf), task_id_string(&silent),]),
        "#1233 restart restores the original task ids"
    );
    for task in listed {
        assert_eq!(task.status, TaskStatus::Completed, "#1233 restored status");
        assert_eq!(task.exit_code, Some(Some(0)), "#1233 restored exit code");
        assert!(task.pid.is_some(), "#1233 restored pid");
        assert!(task.ended_at.is_some(), "#1233 restored endedAt");
        let expected_bytes = if task.task_id == printf.task_id { 6 } else { 0 };
        assert_eq!(
            task.output_bytes, expected_bytes,
            "#1233 restored output bytes"
        );
        let read = runtime2
            .get_output(
                &task.task_id,
                &JsValue::Number(0.0),
                &JsValue::Undefined,
                None,
            )
            .expect("#1233 restart reads output");
        let content = if task.task_id == printf.task_id {
            "abcdef"
        } else {
            ""
        };
        assert_eq!(
            (
                read.content.as_str(),
                read.next_offset,
                read.total_bytes,
                read.truncated
            ),
            (content, expected_bytes as f64, expected_bytes as f64, false),
            "#1233 restart output content"
        );
    }
    let mut statuses = events.lock().unwrap().clone();
    statuses.sort();
    assert_eq!(
        (statuses.len(), second_events.lock().unwrap().len()),
        (
            case["firstRuntimeEvents"]
                .as_array()
                .expect("#1233 oracle first events")
                .len(),
            case["secondRuntimeEvents"]
                .as_u64()
                .expect("#1233 oracle second events") as usize,
        ),
        "#1233 completion events fire only for the owning runtime"
    );
    runtime2.dispose().await.expect("#1233 restored dispose");
}

/// A6.5-BACKGROUND-PERSIST-001-R2 (D3/D4): a crash-style second runtime reads
/// state the first still owns. The durable six bytes are readable, the task
/// itself is unknown and cannot be waited or stopped.
#[tokio::test]
async fn crashed_running_task_recovers_as_unknown_with_durable_output() {
    let case = persist_case("crashed-running-recovery");
    let scratch = scratch::Scratch::new("1233-crashed-running");
    let state = scratch.path().join("state");
    let runtime = configured_runtime(&state, None, None);
    let task = start_command(&runtime, "printf abcdef; sleep 30", scratch.path())
        .await
        .expect("#1233 start the sleeping producer");
    let snapshot_path = state.join("state.json");
    let task_id = task_id_string(&task);

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut running = None;
    while running.is_none() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if let Ok(parsed) = std::fs::read_to_string(&snapshot_path) {
            let parsed: Value = serde_json::from_str(&parsed).expect("#1233 snapshot JSON");
            if let Some(entry) = parsed["tasks"]
                .as_array()
                .expect("#1233 snapshot tasks")
                .iter()
                .find(|entry| entry["task"]["taskId"] == task_id)
                && entry["task"]["status"] == "running"
                && entry["output"]["totalBytes"] == 6
                && entry["output"]["persisted"] == true
            {
                running = Some(entry.clone());
            }
        }
    }
    let running = running.expect("#1233 running snapshot with six durable bytes");
    let oracle_running = &case["runningSnapshot"];
    assert_eq!(running["task"]["status"], oracle_running["status"]);
    assert_eq!(
        running["task"]["exitCode"].is_null(),
        oracle_running["exitCode"].is_null()
    );
    assert!(running["task"]["pid"].is_number(), "#1233 running pid");
    assert_eq!(
        running["task"]["interrupted"], oracle_running["interrupted"],
        "#1233 running interrupted"
    );
    assert_eq!(
        running["task"]["outputBytes"], oracle_running["outputBytes"],
        "#1233 running output bytes"
    );
    assert_eq!(
        running["output"]["totalBytes"], oracle_running["output"]["totalBytes"],
        "#1233 running durable bytes"
    );
    assert_eq!(
        running["output"]["persisted"], oracle_running["output"]["persisted"],
        "#1233 running persisted flag"
    );

    let runtime2 = configured_runtime(&state, None, None);
    let restored = runtime2
        .get(&task.task_id, None)
        .expect("#1233 restart restores the crashed task");
    let oracle_restored = &case["restored"][0];
    assert_eq!(
        restored.status.as_str(),
        oracle_restored["status"]
            .as_str()
            .expect("#1233 oracle status"),
        "#1233 crashed task restores as unknown"
    );
    assert_eq!(
        (
            restored.pid.is_some(),
            restored.exit_code.is_none(),
            restored.ended_at.is_none()
        ),
        (
            oracle_restored["hasPid"]
                .as_bool()
                .expect("#1233 oracle pid"),
            oracle_restored["exitCode"] == "absent",
            oracle_restored["endedAt"] == "absent",
        ),
        "#1233 unknown task keeps no live claims"
    );
    assert_eq!(
        restored.output_bytes as u64,
        oracle_restored["outputBytes"]
            .as_u64()
            .expect("#1233 oracle bytes"),
        "#1233 unknown task keeps its output bytes"
    );
    let read = runtime2
        .get_output(
            &task.task_id,
            &JsValue::Number(0.0),
            &JsValue::Undefined,
            None,
        )
        .expect("#1233 unknown task output is readable");
    let oracle_read = &case["read"];
    assert_eq!(
        (
            read.content.as_str(),
            read.next_offset,
            read.total_bytes,
            read.truncated
        ),
        (
            oracle_read["content"]
                .as_str()
                .expect("#1233 oracle content"),
            oracle_read["nextOffset"]
                .as_f64()
                .expect("#1233 oracle offset"),
            oracle_read["totalBytes"]
                .as_f64()
                .expect("#1233 oracle bytes"),
            oracle_read["truncated"]
                .as_bool()
                .expect("#1233 oracle truncated"),
        ),
        "#1233 crashed output content"
    );
    let waited = timeout(
        Duration::from_secs(10),
        runtime2.wait(&task.task_id, &JsValue::Undefined, None, None),
    )
    .await
    .expect("#1233 unknown wait deadline")
    .expect("#1233 unknown task stays registered")
    .expect("#1233 wait for unknown task");
    assert_eq!(
        waited.outcome,
        case["waitOutcome"]
            .as_str()
            .expect("#1233 oracle wait outcome"),
        "#1233 unknown wait outcome"
    );
    let stop_error = runtime2
        .stop(&task.task_id, &JsValue::Undefined, None)
        .await
        .expect_err("#1233 unknown task cannot be stopped");
    let oracle_stop = &case["stopError"];
    assert_eq!(
        stop_error.name, "BackgroundTaskStateUnknownError",
        "#1233 stop error name"
    );
    assert_eq!(
        message(&stop_error),
        format!(
            "Task {} has an unknown restored state and cannot be waited or stopped.",
            task_id
        ),
        "#1233 stop error message"
    );
    assert_eq!(
        stop_error.name, oracle_stop["name"],
        "#1233 stop error name matches the baseline"
    );

    // Cleanup through the owning runtime: stop the sleeping child, settle and
    // dispose both runtimes so nothing is left running.
    runtime
        .stop(&task.task_id, &JsValue::Undefined, None)
        .await
        .expect("#1233 owning runtime stops the child");
    let settled = timeout(
        Duration::from_secs(10),
        runtime.wait(&task.task_id, &JsValue::Undefined, None, None),
    )
    .await
    .expect("#1233 settle deadline")
    .expect("#1233 task stays registered")
    .expect("#1233 wait for the stopped task");
    assert_eq!(
        settled.task.status,
        TaskStatus::Cancelled,
        "#1233 stopped child cancels"
    );
    runtime.dispose().await.expect("#1233 owning dispose");
    runtime2.dispose().await.expect("#1233 restarted dispose");
}

/// Build a runtime over `state` and return the panic message its constructor
/// produced. The catch keeps the failure inside this test's own panic block,
/// so a negative control can bind to the `#1233` marker (a bare
/// `#[should_panic]` body never carries one).
fn constructor_panic_message(state: &Path, marker: &str) -> String {
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        configured_runtime(state, None, None);
    }))
    .expect_err(marker);
    caught
        .downcast::<String>()
        .map(|payload| *payload)
        .unwrap_or_else(|payload| match payload.downcast::<&str>() {
            Ok(text) => (*text).to_owned(),
            Err(_) => panic!("{marker}: panic payload is not a message"),
        })
}

/// A6.5-BACKGROUND-PERSIST-001-R2 (D2): a restart against output that no longer
/// matches the durable claim fails closed in the constructor.
#[tokio::test]
async fn restart_output_length_mismatch_fails_closed() {
    let scratch = scratch::Scratch::new("1233-restart-mismatch");
    let state = scratch.path().join("state");
    let runtime = configured_runtime(&state, None, None);
    let task = start_and_wait(&runtime, "printf abcdef", scratch.path()).await;
    runtime
        .dispose()
        .await
        .expect("#1233 mismatch setup dispose");
    let spill = state
        .join("output")
        .join(format!("{}.log", task_id_string(&task)));
    assert_eq!(
        std::fs::read(&spill).expect("#1233 spill log").len(),
        6,
        "#1233 six bytes on disk before the mismatch"
    );
    std::fs::write(&spill, b"abcdefX").expect("#1233 corrupt the spill log");
    let oracle_message = persist_case("restart-mismatch")["restart"]["message"]
        .as_str()
        .expect("#1233 oracle restart message")
        .to_owned();
    assert!(
        oracle_message.contains("has 7 bytes; expected 6."),
        "#1233 the baseline restart error reports the mismatch"
    );
    let payload =
        constructor_panic_message(&state, "#1233 a length mismatch must fail the restart");
    assert!(
        payload.contains("has 7 bytes; expected 6."),
        "#1233 mismatch message: {payload}"
    );
}

/// A6.5-BACKGROUND-PERSIST-001-R2 (D2): a missing spill log fails the restart.
#[tokio::test]
async fn restart_missing_output_fails_closed() {
    let scratch = scratch::Scratch::new("1233-restart-missing");
    let state = scratch.path().join("state");
    let runtime = configured_runtime(&state, None, None);
    let task = start_and_wait(&runtime, "printf abcdef", scratch.path()).await;
    runtime
        .dispose()
        .await
        .expect("#1233 missing setup dispose");
    let spill = state
        .join("output")
        .join(format!("{}.log", task_id_string(&task)));
    std::fs::remove_file(&spill).expect("#1233 remove the spill log");
    let payload =
        constructor_panic_message(&state, "#1233 a missing spill log must fail the restart");
    assert!(
        payload.contains("Could not restore task output from")
            && payload.ends_with(&format!("{}.log.", task_id_string(&task))),
        "#1233 missing-output message: {payload}"
    );
}

/// A6.5-BACKGROUND-PERSIST-001-R2 (D6, registered deviation): when every spill
/// write fails, the bytes stay readable in memory, but the snapshot never
/// claims durable output the restart could not honor. The baseline records
/// `completed`/6/`persisted:true`; Rust keeps the start-time `pending`/0
/// claim. No phantom output is ever served on either side.
#[tokio::test]
async fn spill_write_failure_keeps_snapshot_without_durable_claims() {
    let case = persist_case("spill-write-failure");
    let scratch = scratch::Scratch::new("1233-spill-write-failure");
    let state = scratch.path().join("state");
    std::fs::create_dir_all(&state).expect("#1233 create state directory");
    std::fs::write(state.join("output"), "blocked").expect("#1233 block the spill directory");
    let (sink, events) = completion_sink();
    let runtime = configured_runtime(&state, None, Some(sink));
    let task = start_and_wait(&runtime, "printf abcdef", scratch.path()).await;

    let in_memory = runtime
        .get_output(
            &task.task_id,
            &JsValue::Number(0.0),
            &JsValue::Undefined,
            None,
        )
        .expect("#1233 in-memory read");
    let oracle_read = &case["inMemory"];
    assert_eq!(
        (
            in_memory.content.as_str(),
            in_memory.next_offset,
            in_memory.total_bytes,
            in_memory.truncated
        ),
        (
            oracle_read["content"]
                .as_str()
                .expect("#1233 oracle content"),
            oracle_read["nextOffset"]
                .as_f64()
                .expect("#1233 oracle offset"),
            oracle_read["totalBytes"]
                .as_f64()
                .expect("#1233 oracle bytes"),
            oracle_read["truncated"]
                .as_bool()
                .expect("#1233 oracle truncated"),
        ),
        "#1233 failed spill writes keep the in-memory view"
    );
    runtime
        .dispose()
        .await
        .expect("#1233 dispose tolerates failed spill writes");
    assert_eq!(
        *events.lock().unwrap(),
        case["completionEvents"]
            .as_array()
            .expect("#1233 oracle events")
            .iter()
            .map(|status| status.as_str().expect("#1233 event status").to_owned())
            .collect::<Vec<_>>(),
        "#1233 the completion still notifies"
    );

    let on_disk = read_snapshot_file(&state.join("state.json"));
    let entry = &on_disk["tasks"]
        .as_array()
        .expect("#1233 snapshot tasks")
        .iter()
        .find(|entry| entry["task"]["taskId"] == task_id_string(&task))
        .expect("#1233 start-time snapshot survives");
    assert_eq!(
        entry["task"]["status"], "pending",
        "#1233 unsettled claim only"
    );
    assert_eq!(
        entry["output"]["totalBytes"], 0,
        "#1233 no durable output is claimed"
    );
    assert_eq!(
        entry["output"]["persisted"], false,
        "#1233 persisted stays false"
    );
}

/// A6.5-BACKGROUND-PERSIST-001-R2 (D2): a restart against state whose spill
/// directory is unusable fails closed with the baseline's exact restart
/// error, whatever the snapshot claims. The baseline reaches it because it
/// already claimed `persisted:true`; Rust reaches it because the restore
/// refuses to read output through a non-directory path even for an empty
/// expected log.
#[tokio::test]
async fn spill_write_failure_restart_fails_closed() {
    let case = persist_case("spill-write-failure");
    assert_eq!(
        case["restart"]["message"]
            .as_str()
            .expect("#1233 oracle restart message")
            .rsplit('/')
            .next()
            .unwrap(),
        "task-printf.log.",
        "#1233 the baseline restart error names the spill log"
    );
    let scratch = scratch::Scratch::new("1233-spill-write-restart");
    let state = scratch.path().join("state");
    std::fs::create_dir_all(&state).expect("#1233 create state directory");
    std::fs::write(state.join("output"), "blocked").expect("#1233 block the spill directory");
    let runtime = configured_runtime(&state, None, None);
    let _ = start_and_wait(&runtime, "printf abcdef", scratch.path()).await;
    runtime
        .dispose()
        .await
        .expect("#1233 spill-write restart setup dispose");
    let payload = constructor_panic_message(
        &state,
        "#1233 an unusable spill directory must fail the restart",
    );
    assert!(
        payload.contains("Could not restore task output from"),
        "#1233 unusable-spill message: {payload}"
    );
}

/// A6.5-BACKGROUND-PERSIST-001-R2 (D2): a snapshot entry whose task cannot be
/// restored fails the constructor; dropping it would silently lose the task.
struct InvalidTaskStore {
    snapshot: PersistedBackgroundTask,
}

impl BackgroundTaskSnapshotStore for InvalidTaskStore {
    fn load(&self) -> Vec<PersistedBackgroundTask> {
        vec![self.snapshot.clone()]
    }
    fn write(
        &self,
        _snapshot: PersistedBackgroundTask,
        _seq: u64,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
    fn flush(&self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

/// A6.5-BACKGROUND-PERSIST-001-R2 (D2): an unrestorable task entry fails the
/// restart instead of being skipped. The JSON store's own loader cannot reach
/// this (it validates first), so the entry arrives through a custom store.
#[tokio::test]
async fn restart_invalid_task_fails_closed() {
    let scratch = scratch::Scratch::new("1233-restart-invalid-task");
    let state = scratch.path().join("state");
    let mut snapshot = PersistedBackgroundTask {
        task: pilotdeck_gateway::runtime::background_snapshot::persist_task(&BackgroundBashTask {
            task_id: "bogus".into(),
            agent_id: JsValue::Undefined,
            session_id: JsValue::Undefined,
            kind: "bash".into(),
            command: "true".into(),
            cwd: "/".into(),
            pid: None,
            status: TaskStatus::Completed,
            exit_code: Some(Some(0)),
            interrupted: false,
            started_at: 0.0,
            ended_at: Some(0.0),
            output_bytes: 0,
        }),
        output: PersistedOutput {
            total_bytes: 0,
            persisted: false,
        },
    };
    snapshot.task.status = "bogus".into();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
            disk_spill_dir: Some(state.clone()),
            snapshot_store: Some(Arc::new(InvalidTaskStore { snapshot })),
            ..Default::default()
        });
    }))
    .expect_err("#1233 an unrestorable task must fail the restart");
    let payload = caught
        .downcast::<String>()
        .map(|payload| *payload)
        .unwrap_or_else(|payload| match payload.downcast::<&str>() {
            Ok(text) => (*text).to_owned(),
            Err(_) => panic!("#1233 an unrestorable task: no panic message"),
        });
    assert!(
        payload.contains("Background task snapshot restore failed:")
            && payload.contains("Invalid task status: bogus"),
        "#1233 unrestorable-task message: {payload}"
    );
}

/// A6.5-BACKGROUND-PERSIST-001-R3 (P1-2): a corrupted snapshot file fails the
/// store constructor closed on every defect the baseline rejects. The TS
/// goldens come from the fixture's `corrupt-*` cases (real Node v22.22.0);
/// two Rust message differences are registered in the R3 ledger: the bad-JSON
/// rejection carries the parser detail, and the invalid-status rejection
/// phrases the same failure differently.
#[test]
fn corrupted_snapshot_file_fails_closed() {
    let task = |status: &str| {
        serde_json::json!({
            "taskId": "task-a", "type": "local_bash", "kind": "bash",
            "command": "printf abcdef", "cwd": "/",
            "status": status, "completionStatusSentInAttachment": false,
            "lastReportedTotalLines": 0, "isBackgrounded": true,
            "interrupted": false,
            "startedAt": "2026-10-24T00:00:00.000Z",
            "endedAt": "2026-10-24T00:00:01.000Z", "exitCode": 0,
            "outputBytes": 0,
        })
    };
    let output = serde_json::json!({ "totalBytes": 0, "persisted": false });
    let bad_json = "{ not json".to_owned();
    let version_2 =
        serde_json::json!({ "version": 2, "tasks": [{ "task": task("completed"), "output": output }] })
            .to_string();
    let duplicate = serde_json::json!({
        "version": 1,
        "tasks": [
            { "task": task("completed"), "output": output },
            { "task": task("completed"), "output": output },
        ],
    })
    .to_string();
    let invalid_status =
        serde_json::json!({ "version": 1, "tasks": [{ "task": task("banana"), "output": output }] })
            .to_string();
    let cases = [
        ("bad-json", bad_json),
        ("version-2", version_2),
        ("duplicate-id", duplicate),
        ("invalid-status", invalid_status),
    ];
    for (variant, payload) in cases {
        let scratch = scratch::Scratch::new("1233-corrupt-snapshot");
        let state = scratch.path().join("state");
        std::fs::create_dir_all(&state).expect("#1233 create state directory");
        std::fs::write(state.join("state.json"), payload)
            .expect("#1233 write the corrupted snapshot");
        let error = match JsonFileBackgroundTaskSnapshotStore::new(state.join("state.json")) {
            Ok(_) => panic!("#1233 corrupt-{variant} must fail the store constructor"),
            Err(error) => error.to_string(),
        };
        let path = state.join("state.json").display().to_string();
        // The exact Rust rejection text for this defect.
        let expected = match variant {
            "bad-json" => format!(
                "Background task snapshot {path} is not valid JSON; \
                 recovery is required: Invalid JSON: key must be a string at line 1 column 3"
            ),
            "version-2" => format!(
                "Background task snapshot {path} has an unsupported schema; recovery is required."
            ),
            "duplicate-id" => format!(
                "Background task snapshot {path} repeats taskId \"task-a\"; recovery is required."
            ),
            "invalid-status" => format!(
                "Background task snapshot {path} has invalid task at index 0: status is invalid."
            ),
            _ => unreachable!(),
        };
        assert_eq!(
            error, expected,
            "#1233 corrupt-{variant} fails closed with the exact Rust message"
        );
        // The exact TS golden, as the fixture generator normalized it.
        let oracle = persist_case(&format!("corrupt-{variant}"));
        let ts_message = oracle["restart"]["message"]
            .as_str()
            .expect("#1233 oracle corrupt message");
        let ts_expected = match variant {
            "bad-json" => {
                "Background task snapshot <state>/state.json is not valid JSON; \
                 recovery is required."
            }
            "version-2" => {
                "Background task snapshot <state>/state.json has an unsupported schema; \
                 recovery is required."
            }
            "duplicate-id" => {
                "Background task snapshot <state>/state.json repeats taskId \"task-a\"; \
                 recovery is required."
            }
            "invalid-status" => {
                "Background task snapshot <state>/state.json has an invalid task at index 0; \
                 recovery is required."
            }
            _ => unreachable!(),
        };
        assert_eq!(
            ts_message, ts_expected,
            "#1233 corrupt-{variant} TS golden is pinned"
        );
    }
}

/// A6.5-BACKGROUND-PERSIST-001-R2 (D5, registered deviation): a terminal
/// settle whose snapshot write fails. The baseline suppresses the completion
/// notification and reports through `onDiagnostic`; Rust notifies first and
/// persists fire-and-forget, so the event fires. The terminal claim never
/// lands: the durable state stays at whatever unsettled claim preceded the
/// failed write (the start-time `pending`/0 write, or the per-append
/// `running`/6 write — the two race in the baseline probe too), so a restart
/// never sees a completed task it could not have honored.
#[tokio::test]
async fn settle_write_failure_still_notifies_and_leaves_no_terminal_claim() {
    persist_case("settle-ok");
    let case = persist_case("settle-write-failure");
    assert_eq!(
        case["completionEvents"],
        serde_json::json!([]),
        "#1233 the baseline suppresses the completion notification (D5)"
    );
    assert_eq!(
        case["diagnostics"],
        serde_json::json!(["#1233 settle write refused"]),
        "#1233 the baseline reports the failed settle write (D5)"
    );
    let scratch = scratch::Scratch::new("1233-settle-write-failure");
    let state = scratch.path().join("state");
    let inner = Arc::new(
        JsonFileBackgroundTaskSnapshotStore::new(state.join("state.json"))
            .expect("#1233 create snapshot store"),
    );
    let failing: Arc<dyn BackgroundTaskSnapshotStore> =
        Arc::new(TerminalWriteFailureStore { inner });
    let (sink, events) = completion_sink();
    let runtime = configured_runtime(&state, Some(failing), Some(sink));
    let task = start_command(&runtime, "printf abcdef", scratch.path())
        .await
        .expect("#1233 start before settle failure");

    let _ = timeout(
        Duration::from_secs(30),
        runtime.wait(&task.task_id, &JsValue::Undefined, None, None),
    )
    .await
    .expect("#1233 settle deadline")
    .expect("#1233 task stays registered")
    .expect("#1233 wait for the settled task");
    assert_eq!(
        *events.lock().unwrap(),
        vec!["completed".to_owned()],
        "#1233 D5 deviation: Rust notifies despite the failed settle write"
    );

    runtime
        .dispose()
        .await
        .expect("#1233 dispose tolerates the failed settle write");
    let on_disk = read_snapshot_file(&state.join("state.json"));
    let entry = &on_disk["tasks"]
        .as_array()
        .expect("#1233 snapshot tasks")
        .iter()
        .find(|entry| entry["task"]["taskId"] == task_id_string(&task))
        .expect("#1233 unsettled snapshot survives the failed settle");
    assert_ne!(
        entry["task"]["status"], "completed",
        "#1233 the terminal claim never lands"
    );
}

/// A6.5-BACKGROUND-PANIC-001-R2 (D1): the project-level resource dispose
/// propagates the flush rejection to its caller, like `disposeRuntime`'s
/// awaited bundle. The JS error shape rides along for staging consumers.
#[tokio::test]
async fn background_resources_dispose_propagates_the_flush_rejection() {
    let scratch = scratch::Scratch::new("1233-resources-dispose");
    let state = scratch.path().join("state");
    let inner = Arc::new(
        JsonFileBackgroundTaskSnapshotStore::new(state.join("state.json"))
            .expect("#1233 create snapshot store"),
    );
    let runtime = configured_runtime(
        &state,
        Some(Arc::new(FlushFailureStore { inner, sync: false })),
        None,
    );
    let _task = start_and_wait(&runtime, "printf abcdef", scratch.path()).await;
    let resources = BackgroundResources {
        tasks: runtime,
        subagents: BackgroundSubagentRuntime::default(),
    };
    let error = resources
        .dispose()
        .await
        .expect_err("#1233 resources dispose propagates the flush failure");
    assert!(
        error.message.contains("#1233 flush refused"),
        "#1233 resources dispose message: {}",
        error.message
    );
    let js_error = error
        .js_error
        .expect("#1233 the flush failure keeps its JS error shape");
    assert_eq!(
        (
            String::from_utf16_lossy(js_error.message.units()),
            js_error.requires_host
        ),
        ("#1233 flush refused".to_owned(), false),
        "#1233 the JS error message and host requirement survive staging"
    );
}

/// A snapshot store whose terminal `write` refuses, matching the baseline's
/// `settle-write-failure` probe: unsettled writes still succeed, so the
/// start-time claim lands on disk.
struct TerminalWriteFailureStore {
    inner: Arc<JsonFileBackgroundTaskSnapshotStore>,
}

impl BackgroundTaskSnapshotStore for TerminalWriteFailureStore {
    fn load(&self) -> Vec<PersistedBackgroundTask> {
        self.inner.load()
    }
    fn write(
        &self,
        snapshot: PersistedBackgroundTask,
        seq: u64,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        if matches!(
            snapshot.task.status.as_str(),
            "completed" | "failed" | "cancelled"
        ) {
            return Box::pin(async { Err(io::Error::other("#1233 settle write refused")) });
        }
        self.inner.write(snapshot, seq)
    }
    fn flush(&self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        self.inner.flush()
    }
}
