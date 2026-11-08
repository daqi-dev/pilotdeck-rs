//! Drive the actual persist entry path while a later native output tail is held.
use super::*;
use crate::runtime::{
    background::native_output::NativeOutputFile,
    background_snapshot::{BackgroundTaskSnapshotStore, PersistedBackgroundTask},
};
use std::{
    fs::{self, OpenOptions},
    io,
    task::Poll,
    time::Duration,
};

#[derive(Default)]
struct CaptureStore {
    writes: Mutex<Vec<PersistedBackgroundTask>>,
    failures: Mutex<Vec<String>>,
}
impl BackgroundTaskSnapshotStore for CaptureStore {
    fn load(&self) -> Vec<PersistedBackgroundTask> {
        Vec::new()
    }
    fn report_failure(&self, message: String) {
        self.failures.lock().unwrap().push(message);
    }
    fn write(
        &self,
        snapshot: PersistedBackgroundTask,
        _seq: u64,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.writes.lock().unwrap().push(snapshot);
            Ok(())
        })
    }
    fn flush(&self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[tokio::test]
async fn persist_entry_captures_native_prefix_before_awaiting_and_does_not_poison_archive() {
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "pdrs-native-persist-entry-{}",
        uuid::Uuid::new_v4()
    )));
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create_new(true)
        .open(&scratch.0)
        .unwrap();
    let output = OutputStore::with_native_file(
        1024,
        NativeOutputFile {
            file,
            path: scratch.0.clone(),
            allow_prefix: false,
            link_check: Arc::new(|| true),
        },
    );
    let entry = Arc::new(Entry {
        state: Mutex::new(EntryState {
            task: BackgroundBashTask {
                task_id: "00000000-0000-4000-8000-000000000001".into(),
                agent_id: JsValue::Undefined,
                session_id: JsValue::Undefined,
                kind: "bash".into(),
                command: "fixture only".into(),
                cwd: "/".into(),
                pid: None,
                status: TaskStatus::Running,
                exit_code: None,
                interrupted: false,
                started_at: 0.0,
                ended_at: None,
                output_bytes: 0,
            },
            output,
            child: None,
            persist_seq: 0,
            output_limit_exceeded: false,
            restored_failure: None,
        }),
        done: watch::channel(false).0,
        closed: watch::channel(false).0,
        never_exits: false,
        native_persist: std::sync::OnceLock::new(),
    });
    let capture = Arc::new(CaptureStore::default());
    let store: Arc<dyn BackgroundTaskSnapshotStore> = capture.clone();
    let (entered_first, release_first) = lock(&entry.state).output.gate_next_write();
    let (seq, _) = entry.append(b"first", None);
    tokio::time::timeout(Duration::from_secs(2), entered_first)
        .await
        .unwrap()
        .unwrap();
    let mut persist = Box::pin(BackgroundTaskRuntime::persist_entry_static(
        &store, &entry, seq,
    ));
    let was_pending =
        std::future::poll_fn(|cx| Poll::Ready(persist.as_mut().poll(cx).is_pending())).await;
    assert!(
        was_pending,
        "persist reached its captured first-write checkpoint"
    );
    let (entered_second, release_second) = lock(&entry.state).output.gate_next_write();
    entry.append(b"second", None);
    release_first.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), entered_second)
        .await
        .unwrap()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), persist).await;
    let live_pending = !lock(&entry.state).output.has_durable_output();
    release_second.send(()).unwrap();
    let flush = lock(&entry.state).output.flush();
    tokio::time::timeout(Duration::from_secs(2), flush)
        .await
        .unwrap();
    assert!(
        result.unwrap().is_ok(),
        "later pending bytes must not poison a persisted earlier prefix"
    );
    assert!(live_pending);
    assert!(capture.failures.lock().unwrap().is_empty());
    let writes = capture.writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].task.output_bytes, 5);
    assert_eq!(writes[0].output.total_bytes, 5);
    assert!(writes[0].output.persisted);
    assert_eq!(fs::read(&scratch.0).unwrap(), b"firstsecond");
}
