//! Background task snapshot persistence (A6.5-BACKGROUND-PERSIST-001).
//!
//! Ported from `task/storage/BackgroundTaskSnapshotStore.ts`: project-scoped
//! durable metadata for detached tasks, enabling restart recovery.

use crate::runtime::background::{BackgroundBashTask, iso};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::{sync::oneshot, task::spawn_blocking};

const SNAPSHOT_VERSION: u32 = 1;

/// `PersistedBackgroundTask["output"]`.
#[derive(Clone, Debug)]
pub struct PersistedOutput {
    pub total_bytes: u64,
    pub persisted: bool,
}

/// `PersistedBackgroundTask["task"]` with ISO date strings.
#[derive(Clone, Debug)]
pub struct PersistedTask {
    pub task_id: String,
    pub task_type: String,
    pub kind: String,
    pub command: String,
    pub cwd: String,
    pub status: String,
    pub completion_status_sent_in_attachment: bool,
    pub last_reported_total_lines: u64,
    pub is_backgrounded: bool,
    pub interrupted: bool,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub pid: Option<u32>,
    pub exit_code: Option<Option<i32>>,
    pub output_bytes: u64,
}

/// `PersistedBackgroundTask`.
#[derive(Clone, Debug)]
pub struct PersistedBackgroundTask {
    pub task: PersistedTask,
    pub output: PersistedOutput,
}

/// Snapshot file schema.
#[derive(Clone, Debug)]
struct SnapshotFile {
    version: u32,
    tasks: Vec<PersistedBackgroundTask>,
}

/// `BackgroundTaskSnapshotStore` trait.
///
/// #1233 fix-r1 (P0-1): `write` carries the event-time sequence number
/// assigned under the entry lock when the persisted event happened. A write
/// whose sequence is older than the newest accepted sequence for the same
/// task must be dropped by the store, so a slow persist cannot overwrite a
/// newer snapshot. `u64::MAX` is reserved for the dispose-time final write,
/// which is never dropped.
pub trait BackgroundTaskSnapshotStore: Send + Sync {
    fn load(&self) -> Vec<PersistedBackgroundTask>;
    fn write(
        &self,
        snapshot: PersistedBackgroundTask,
        seq: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = io::Result<()>> + Send + '_>>;
    fn flush(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = io::Result<()>> + Send + '_>>;
}

/// `JsonFileBackgroundTaskSnapshotStore`: atomic replace via temp + rename.
#[derive(Clone)]
pub struct JsonFileBackgroundTaskSnapshotStore {
    file_path: PathBuf,
    entries: Arc<Mutex<Vec<PersistedBackgroundTask>>>,
    write_tail: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// #1233 fix-r1: newest accepted sequence per task id.
    accepted: Arc<Mutex<std::collections::HashMap<String, u64>>>,
    #[cfg(test)]
    writes_started: Arc<std::sync::atomic::AtomicUsize>,
}

impl JsonFileBackgroundTaskSnapshotStore {
    pub fn new(file_path: PathBuf) -> io::Result<Self> {
        let entries = load_snapshot_file(&file_path)?;
        Ok(Self {
            file_path,
            entries: Arc::new(Mutex::new(entries)),
            write_tail: Arc::new(Mutex::new(None)),
            accepted: Arc::new(Mutex::new(std::collections::HashMap::new())),
            #[cfg(test)]
            writes_started: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    async fn write_current(&self) -> io::Result<()> {
        #[cfg(test)]
        self.writes_started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let file_path = self.file_path.clone();
        let entries = self.entries.lock().unwrap().clone();

        spawn_blocking(move || {
            let snapshot = SnapshotFile {
                version: SNAPSHOT_VERSION,
                tasks: entries,
            };

            // Manual JSON serialization
            let payload = serialize_snapshot_file(&snapshot)?;

            let temp_path = format!(
                "{}.{}.{}.tmp",
                file_path.display(),
                std::process::id(),
                uuid::Uuid::new_v4()
            );

            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent)?;
            }

            fs::write(&temp_path, payload)?;
            fs::rename(&temp_path, &file_path)?;
            Ok(())
        })
        .await
        .map_err(io::Error::other)?
    }
}

impl BackgroundTaskSnapshotStore for JsonFileBackgroundTaskSnapshotStore {
    fn load(&self) -> Vec<PersistedBackgroundTask> {
        self.entries.lock().unwrap().clone()
    }

    fn write(
        &self,
        snapshot: PersistedBackgroundTask,
        seq: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = io::Result<()>> + Send + '_>> {
        // Accept the update and register its task atomically, before returning
        // a cancellable waiter. Every filesystem write belongs to this chain.
        let mut tail = self.write_tail.lock().unwrap();
        // #1233 fix-r1 (P0-1): a write superseded by a newer accepted
        // sequence for the same task is dropped before it can enqueue. The
        // check and the registration both sit under the write-tail lock, where
        // writes serialize, so the decision cannot interleave with another
        // write's registration. Ties pass: an idempotent retry must not lose
        // the write.
        {
            let mut accepted = self.accepted.lock().unwrap();
            let task_id = snapshot.task.task_id.clone();
            if seq < accepted.get(&task_id).copied().unwrap_or(0) {
                return Box::pin(async { Ok(()) });
            }
            accepted.insert(task_id, seq);
        }
        // Update in-memory map
        {
            let mut entries = self.entries.lock().unwrap();
            if let Some(pos) = entries
                .iter()
                .position(|e| e.task.task_id == snapshot.task.task_id)
            {
                entries[pos] = snapshot;
            } else {
                entries.push(snapshot);
            }
        }

        let prev_task = tail.take();
        let store = self.clone();
        let (done, completion) = oneshot::channel();

        *tail = Some(tokio::spawn(async move {
            if let Some(prev) = prev_task {
                let _ = prev.await;
            }
            let _ = done.send(store.write_current().await);
        }));

        Box::pin(async move { completion.await.map_err(io::Error::other)? })
    }

    fn flush(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = io::Result<()>> + Send + '_>> {
        // Keep the barrier in the chain so concurrent writes and flushes
        // cannot bypass earlier writes, even if this waiter is dropped.
        let mut tail = self.write_tail.lock().unwrap();
        let prev_task = tail.take();
        let (done, completion) = oneshot::channel();
        *tail = Some(tokio::spawn(async move {
            let result = match prev_task {
                Some(task) => task.await.map_err(io::Error::other),
                None => Ok(()),
            };
            let _ = done.send(result);
        }));
        Box::pin(async move { completion.await.map_err(io::Error::other)? })
    }
}

fn serialize_snapshot_file(snapshot: &SnapshotFile) -> io::Result<String> {
    use serde_json::{Value, json};

    let tasks: Vec<Value> = snapshot
        .tasks
        .iter()
        .map(|entry| {
            let task = &entry.task;
            let mut task_obj = json!({
                "taskId": task.task_id,
                "type": task.task_type,
                "kind": task.kind,
                "command": task.command,
                "cwd": task.cwd,
                "status": task.status,
                "completionStatusSentInAttachment": task.completion_status_sent_in_attachment,
                "lastReportedTotalLines": task.last_reported_total_lines,
                "isBackgrounded": task.is_backgrounded,
                "interrupted": task.interrupted,
                "startedAt": task.started_at,
                "outputBytes": task.output_bytes,
            });

            let task_map = task_obj.as_object_mut().unwrap();

            if let Some(ended_at) = &task.ended_at {
                task_map.insert("endedAt".to_string(), json!(ended_at));
            }
            if let Some(agent_id) = &task.agent_id {
                task_map.insert("agentId".to_string(), json!(agent_id));
            }
            if let Some(session_id) = &task.session_id {
                task_map.insert("sessionId".to_string(), json!(session_id));
            }
            if let Some(pid) = task.pid {
                task_map.insert("pid".to_string(), json!(pid));
            }
            if let Some(exit_code) = task.exit_code {
                task_map.insert("exitCode".to_string(), json!(exit_code));
            }

            json!({
                "task": task_obj,
                "output": {
                    "totalBytes": entry.output.total_bytes,
                    "persisted": entry.output.persisted,
                }
            })
        })
        .collect();

    let result = json!({
        "version": snapshot.version,
        "tasks": tasks,
    });

    serde_json::to_string(&result).map_err(io::Error::other)
}

fn deserialize_snapshot_file(text: &str) -> io::Result<SnapshotFile> {
    use serde_json::Value;

    let value: Value = serde_json::from_str(text)
        .map_err(|e| io::Error::new(ErrorKind::InvalidData, format!("Invalid JSON: {}", e)))?;

    let obj = value
        .as_object()
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "Root must be an object"))?;

    let version = obj
        .get("version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "Missing or invalid version"))?
        as u32;

    let tasks_array = obj
        .get("tasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "Missing or invalid tasks array"))?;

    let mut tasks = Vec::new();
    for task_value in tasks_array {
        let task_obj = task_value.as_object().ok_or_else(|| {
            io::Error::new(ErrorKind::InvalidData, "Task entry must be an object")
        })?;

        let task_data = task_obj
            .get("task")
            .and_then(|v| v.as_object())
            .ok_or_else(|| {
                io::Error::new(ErrorKind::InvalidData, "Missing or invalid task object")
            })?;

        let output_data = task_obj
            .get("output")
            .and_then(|v| v.as_object())
            .ok_or_else(|| {
                io::Error::new(ErrorKind::InvalidData, "Missing or invalid output object")
            })?;

        let task = PersistedTask {
            task_id: task_data
                .get("taskId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            task_type: task_data
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            kind: task_data
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            command: task_data
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            cwd: task_data
                .get("cwd")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            status: task_data
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            completion_status_sent_in_attachment: task_data
                .get("completionStatusSentInAttachment")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            last_reported_total_lines: task_data
                .get("lastReportedTotalLines")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            is_backgrounded: task_data
                .get("isBackgrounded")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            interrupted: task_data
                .get("interrupted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            started_at: task_data
                .get("startedAt")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            ended_at: task_data
                .get("endedAt")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            agent_id: task_data
                .get("agentId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            session_id: task_data
                .get("sessionId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            pid: task_data
                .get("pid")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32),
            exit_code: task_data.get("exitCode").and_then(|v| {
                if v.is_null() {
                    Some(None)
                } else {
                    v.as_i64().map(|n| Some(n as i32))
                }
            }),
            output_bytes: task_data
                .get("outputBytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        };

        let output = PersistedOutput {
            total_bytes: output_data
                .get("totalBytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            persisted: output_data
                .get("persisted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        };

        tasks.push(PersistedBackgroundTask { task, output });
    }

    Ok(SnapshotFile { version, tasks })
}

fn load_snapshot_file(file_path: &Path) -> io::Result<Vec<PersistedBackgroundTask>> {
    let text = match fs::read_to_string(file_path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(io::Error::other(format!(
                "Could not read background task snapshot {}: {}",
                file_path.display(),
                e
            )));
        }
    };

    let snapshot = deserialize_snapshot_file(&text).map_err(|e| {
        io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "Background task snapshot {} is not valid JSON; recovery is required: {}",
                file_path.display(),
                e
            ),
        )
    })?;

    if snapshot.version != SNAPSHOT_VERSION {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "Background task snapshot {} has an unsupported schema; recovery is required.",
                file_path.display()
            ),
        ));
    }

    validate_tasks(&snapshot.tasks, file_path)?;
    Ok(snapshot.tasks)
}

fn validate_tasks(tasks: &[PersistedBackgroundTask], file_path: &Path) -> io::Result<()> {
    let mut seen = std::collections::HashSet::new();

    for (index, entry) in tasks.iter().enumerate() {
        validate_snapshot(entry, file_path, index)?;

        if !seen.insert(&entry.task.task_id) {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                format!(
                    "Background task snapshot {} repeats taskId {:?}; recovery is required.",
                    file_path.display(),
                    entry.task.task_id
                ),
            ));
        }
    }

    Ok(())
}

fn validate_snapshot(
    entry: &PersistedBackgroundTask,
    file_path: &Path,
    index: usize,
) -> io::Result<()> {
    let task = &entry.task;
    let output = &entry.output;

    if task.task_id.is_empty() {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "Background task snapshot {} has invalid task at index {}: taskId must be non-empty.",
                file_path.display(),
                index
            ),
        ));
    }

    if task.task_type != "local_bash" {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "Background task snapshot {} has invalid task at index {}: type must be local_bash.",
                file_path.display(),
                index
            ),
        ));
    }

    if task.kind != "bash" && task.kind != "monitor" {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "Background task snapshot {} has invalid task at index {}: kind is invalid.",
                file_path.display(),
                index
            ),
        ));
    }

    if !matches!(
        task.status.as_str(),
        "pending" | "running" | "completed" | "failed" | "cancelled" | "unknown"
    ) {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "Background task snapshot {} has invalid task at index {}: status is invalid.",
                file_path.display(),
                index
            ),
        ));
    }

    if task.output_bytes != output.total_bytes {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "Background task snapshot {} has invalid task at index {}: outputBytes must equal output.totalBytes.",
                file_path.display(),
                index
            ),
        ));
    }

    if output.total_bytes > 0 && !output.persisted {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "Background task snapshot {} has invalid task at index {}: non-empty task output must have durable output storage.",
                file_path.display(),
                index
            ),
        ));
    }

    Ok(())
}

/// Convert `BackgroundBashTask` to `PersistedTask`.
pub fn persist_task(task: &BackgroundBashTask) -> PersistedTask {
    let to_string = |v: &JsValue| match v {
        JsValue::String(s) => String::from_utf16_lossy(s.units()),
        JsValue::Undefined => String::new(),
        _ => String::new(),
    };

    PersistedTask {
        task_id: String::from_utf16_lossy(task.task_id.units()),
        task_type: "local_bash".to_string(),
        kind: to_string(&task.kind),
        command: to_string(&task.command),
        cwd: to_string(&task.cwd),
        status: task.status.as_str().to_string(),
        completion_status_sent_in_attachment: false,
        last_reported_total_lines: 0,
        is_backgrounded: true,
        interrupted: task.interrupted,
        started_at: String::from_utf16_lossy(iso(task.started_at).units()),
        ended_at: task
            .ended_at
            .map(|t| String::from_utf16_lossy(iso(t).units())),
        agent_id: match &task.agent_id {
            JsValue::String(s) if !s.is_empty() => Some(String::from_utf16_lossy(s.units())),
            _ => None,
        },
        session_id: match &task.session_id {
            JsValue::String(s) if !s.is_empty() => Some(String::from_utf16_lossy(s.units())),
            _ => None,
        },
        pid: task.pid,
        exit_code: task.exit_code,
        output_bytes: task.output_bytes,
    }
}

/// Convert `PersistedTask` back to `BackgroundBashTask`.
pub fn restore_task(snapshot: &PersistedTask) -> io::Result<BackgroundBashTask> {
    use crate::runtime::background::{BackgroundBashTask, TaskStatus};
    use pilotdeck_protocol::JsValue;

    let status = match snapshot.status.as_str() {
        "pending" => TaskStatus::Pending,
        "running" => TaskStatus::Running,
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "cancelled" => TaskStatus::Cancelled,
        "unknown" => TaskStatus::Unknown,
        _ => {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                format!("Invalid task status: {}", snapshot.status),
            ));
        }
    };

    let started_at = parse_iso_to_epoch_ms(&snapshot.started_at).ok_or_else(|| {
        io::Error::new(
            ErrorKind::InvalidData,
            format!("Invalid startedAt: {}", snapshot.started_at),
        )
    })?;

    let ended_at = snapshot
        .ended_at
        .as_ref()
        .map(|s| {
            parse_iso_to_epoch_ms(s).ok_or_else(|| {
                io::Error::new(ErrorKind::InvalidData, format!("Invalid endedAt: {}", s))
            })
        })
        .transpose()?;

    Ok(BackgroundBashTask {
        task_id: JsString::from(snapshot.task_id.as_str()),
        agent_id: snapshot
            .agent_id
            .as_ref()
            .map(|s| JsValue::String(JsString::from(s.as_str())))
            .unwrap_or(JsValue::Undefined),
        session_id: snapshot
            .session_id
            .as_ref()
            .map(|s| JsValue::String(JsString::from(s.as_str())))
            .unwrap_or(JsValue::Undefined),
        kind: JsValue::String(JsString::from(snapshot.kind.as_str())),
        command: JsValue::String(JsString::from(snapshot.command.as_str())),
        cwd: JsValue::String(JsString::from(snapshot.cwd.as_str())),
        pid: snapshot.pid,
        status,
        exit_code: snapshot.exit_code,
        interrupted: snapshot.interrupted,
        started_at,
        ended_at,
        output_bytes: snapshot.output_bytes,
    })
}

/// Parse ISO 8601 date string to epoch milliseconds (simplified, follows Date.parse()).
fn parse_iso_to_epoch_ms(iso: &str) -> Option<f64> {
    // Simplified ISO 8601 parser for formats like "2024-10-24T12:34:56.789Z"
    let parts: Vec<&str> = iso.split('T').collect();
    if parts.len() != 2 {
        return None;
    }

    let date_parts: Vec<&str> = parts[0].split('-').collect();
    if date_parts.len() != 3 {
        return None;
    }

    let year: i32 = date_parts[0].parse().ok()?;
    let month: u32 = date_parts[1].parse().ok()?;
    let day: u32 = date_parts[2].parse().ok()?;

    // Validate date components to prevent panic
    if year < 1970 || !(1..=12).contains(&month) || day < 1 {
        return None;
    }

    let time_str = parts[1].trim_end_matches('Z');
    let time_and_ms: Vec<&str> = time_str.split('.').collect();
    let time_parts: Vec<&str> = time_and_ms[0].split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }

    let hour: u32 = time_parts[0].parse().ok()?;
    let minute: u32 = time_parts[1].parse().ok()?;
    let second: u32 = time_parts[2].parse().ok()?;
    let ms: u32 = if time_and_ms.len() > 1 {
        time_and_ms[1].parse().ok()?
    } else {
        0
    };

    // Calculate Unix timestamp (simplified)
    let mut days = 0i64;
    for y in 1970..year {
        days += if is_leap_year(y) { 366 } else { 365 };
    }

    let days_in_month = [
        31,
        if is_leap_year(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];

    // Validate day against month's max days
    if day > days_in_month[(month - 1) as usize] {
        return None;
    }

    for m in 1..month {
        days += days_in_month[(m - 1) as usize] as i64;
    }
    days += (day - 1) as i64;

    let seconds = days * 86400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64;
    let millis = seconds * 1000 + ms as i64;

    Some(millis as f64)
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::atomic::Ordering, time::Duration};

    fn snapshot(task_id: &str) -> PersistedBackgroundTask {
        let mut entry = deserialize_snapshot_file(
            r#"{"version":1,"tasks":[{"task":{
                "taskId":"task","type":"local_bash","kind":"bash",
                "status":"pending","startedAt":"2026-10-24T00:00:00.000Z",
                "outputBytes":0
            },"output":{"totalBytes":0,"persisted":false}}]}"#,
        )
        .unwrap()
        .tasks
        .remove(0);
        entry.task.task_id = task_id.to_owned();
        entry
    }

    #[tokio::test]
    async fn writes_and_flushes_keep_the_chain_when_waiters_are_dropped() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let scratch = crate::test_scratch::Scratch::new("617-snapshot-chain");
            let path = scratch.path().join("snapshot.json");
            let store = JsonFileBackgroundTaskSnapshotStore::new(path.clone()).unwrap();
            let (release, released) = oneshot::channel();
            *store.write_tail.lock().unwrap() = Some(tokio::spawn(async move {
                let _ = released.await;
            }));

            // Neither accepting a write nor a flush depends on polling its
            // returned future. Cancelling either waiter must keep the chain.
            drop(store.write(snapshot("first"), 0));
            drop(store.flush());
            let second = store.write(snapshot("second"), 0);
            let mut flushed = store.flush();
            assert!(
                tokio::time::timeout(Duration::from_millis(100), &mut flushed)
                    .await
                    .is_err(),
                "flush bypassed the gated predecessor"
            );
            assert_eq!(store.writes_started.load(Ordering::SeqCst), 0);
            assert!(!path.exists());

            release.send(()).unwrap();
            second.await.unwrap();
            flushed.await.unwrap();
            assert_eq!(store.writes_started.load(Ordering::SeqCst), 2);
            let loaded = JsonFileBackgroundTaskSnapshotStore::new(path).unwrap();
            let ids: Vec<_> = loaded.load().into_iter().map(|e| e.task.task_id).collect();
            assert_eq!(ids, ["first", "second"]);
        })
        .await
        .expect("bounded snapshot chain probe");
    }

    #[tokio::test]
    async fn write_reports_io_failure_and_the_next_write_can_recover() {
        let scratch = crate::test_scratch::Scratch::new("617-snapshot-write-error");
        let parent = scratch.path().join("store");
        let path = parent.join("snapshot.json");
        let store = JsonFileBackgroundTaskSnapshotStore::new(path.clone()).unwrap();
        fs::write(&parent, b"not a directory").unwrap();

        assert!(store.write(snapshot("task"), 0).await.is_err());
        store.flush().await.unwrap();
        assert_eq!(store.writes_started.load(Ordering::SeqCst), 1);

        fs::remove_file(parent).unwrap();
        let mut completed = snapshot("task");
        completed.task.status = "completed".to_owned();
        store.write(completed, 1).await.unwrap();
        store.flush().await.unwrap();
        assert_eq!(store.writes_started.load(Ordering::SeqCst), 2);
        let loaded = JsonFileBackgroundTaskSnapshotStore::new(path).unwrap();
        assert_eq!(loaded.load()[0].task.status, "completed");
    }

    // #1233 fix-r1 (P0-1): the store-level drop rule. A write whose sequence
    // is older than the newest accepted sequence for the same task must not
    // overwrite the newer snapshot, whatever order the writes land in.
    #[tokio::test]
    async fn superseded_sequence_writes_are_dropped() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let scratch = crate::test_scratch::Scratch::new("1233-snapshot-order");
            let path = scratch.path().join("snapshot.json");
            let store = JsonFileBackgroundTaskSnapshotStore::new(path.clone()).unwrap();

            let mut terminal = snapshot("task");
            terminal.task.status = "completed".to_owned();
            store.write(terminal, 2).await.unwrap();

            // An older running snapshot arriving later must be dropped.
            store.write(snapshot("task"), 1).await.unwrap();
            store.flush().await.unwrap();

            let statuses: Vec<String> = JsonFileBackgroundTaskSnapshotStore::new(path)
                .unwrap()
                .load()
                .into_iter()
                .map(|entry| entry.task.status)
                .collect();
            assert_eq!(
                statuses,
                ["completed".to_owned()],
                "#1233 a superseded snapshot write cannot overwrite the newer state"
            );
        })
        .await
        .expect("bounded snapshot order probe");
    }
}
