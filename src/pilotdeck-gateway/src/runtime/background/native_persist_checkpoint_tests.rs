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

struct TerminalGateStore {
    entered: watch::Sender<bool>,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    fail: bool,
    writes: Mutex<Vec<PersistedBackgroundTask>>,
    failures: Mutex<Vec<String>>,
}
impl BackgroundTaskSnapshotStore for TerminalGateStore {
    fn load(&self) -> Vec<PersistedBackgroundTask> {
        vec![]
    }
    fn report_failure(&self, message: String) {
        self.failures.lock().unwrap().push(message);
    }
    fn write(
        &self,
        snapshot: PersistedBackgroundTask,
        _seq: u64,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        let release = if snapshot.task.status == "completed" {
            self.release.lock().unwrap().take()
        } else {
            None
        };
        Box::pin(async move {
            if let Some(release) = release {
                self.entered.send_replace(true);
                let _ = release.await;
            }
            if self.fail {
                return Err(io::Error::other("fixture terminal write failure"));
            }
            self.writes.lock().unwrap().push(snapshot);
            Ok(())
        })
    }
    fn flush(&self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        Box::pin(async move {
            if self.fail {
                Err(io::Error::other("fixture terminal write failure"))
            } else {
                Ok(())
            }
        })
    }
}
struct NoOutput;
impl super::native_output::NativeOutputStorage for NoOutput {
    fn create(&self, _: &JsString) -> io::Result<NativeOutputFile> {
        Err(io::Error::other("this fixture starts no child"))
    }
    fn restore(&self, _: &JsString, _: u64) -> io::Result<Option<NativeOutputFile>> {
        Err(io::Error::other("this fixture restores no output"))
    }
}
#[tokio::test]
async fn native_done_and_completion_follow_terminal_checkpoint_and_surface_write_failure() {
    for fail in [false, true] {
        let (released, release) = tokio::sync::oneshot::channel();
        let (entered, mut waiting) = watch::channel(false);
        let store = Arc::new(TerminalGateStore {
            entered,
            release: Mutex::new(Some(release)),
            fail,
            writes: Mutex::new(vec![]),
            failures: Mutex::new(vec![]),
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let notified = events.clone();
        let runtime = BackgroundTaskRuntime::new_native(
            BackgroundTaskRuntimeOptions {
                snapshot_store: Some(store.clone()),
                disk_spill_dir: Some(std::env::temp_dir()),
                on_completion: Some(Arc::new(move |event| notified.lock().unwrap().push(event))),
                ..Default::default()
            },
            Arc::new(crate::runtime::tools::sandbox::NativeSandbox::new(
                crate::runtime::project::SandboxMode::DangerFullAccess,
                std::env::temp_dir(),
            )),
            Arc::new(NoOutput),
            NativeTaskLimits::CLI,
        );
        let id: JsString = uuid::Uuid::new_v4().to_string().into();
        let entry = Arc::new(Entry {
            state: Mutex::new(EntryState {
                task: BackgroundBashTask {
                    task_id: id.clone(),
                    agent_id: JsValue::Undefined,
                    session_id: "fixture".into(),
                    kind: "bash".into(),
                    command: "fixture only; no child".into(),
                    cwd: "/".into(),
                    pid: None,
                    status: TaskStatus::Completed,
                    exit_code: Some(Some(0)),
                    interrupted: false,
                    started_at: 0.0,
                    ended_at: Some(1.0),
                    output_bytes: 0,
                },
                output: OutputStore::with_spill(DEFAULT_MEMORY_BYTES, None),
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
        lock(&runtime.inner.state)
            .entries
            .push((id.clone(), entry.clone()));
        runtime.inner.settle(&entry);
        tokio::time::timeout(Duration::from_secs(2), waiting.wait_for(|entered| *entered))
            .await
            .unwrap()
            .unwrap();
        let premature_done = *entry.done.borrow();
        let premature_notification = !events.lock().unwrap().is_empty();
        released.send(()).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            runtime.wait(&id, &JsValue::Number(1000.0), None, None),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        let disposed = tokio::time::timeout(Duration::from_secs(2), runtime.dispose())
            .await
            .unwrap();
        assert!(
            !premature_done,
            "native done cannot outrun a real terminal metadata write"
        );
        assert!(
            !premature_notification,
            "completion notification follows terminal metadata"
        );
        if fail {
            assert_eq!(result.task.status, TaskStatus::Failed);
            assert!(
                runtime
                    .failure_reason(&id, None)
                    .unwrap()
                    .to_utf8()
                    .unwrap()
                    .contains("fixture terminal write failure")
            );
            assert!(!store.failures.lock().unwrap().is_empty());
            assert!(disposed.is_err());
        } else {
            assert_eq!(result.task.status, TaskStatus::Completed);
            assert!(disposed.is_ok());
            assert_eq!(
                store.writes.lock().unwrap().first().unwrap().task.status,
                "completed"
            );
        }
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("status"),
            Some(&JsValue::from(if fail { "failed" } else { "completed" }))
        );
    }
}
