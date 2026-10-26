use super::*;
use crate::runtime::background_snapshot::{
    BackgroundTaskSnapshotStore, PersistedBackgroundTask, PersistedOutput, persist_task,
};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    io,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Wake, Waker},
};
use tokio::sync::oneshot;

async fn bounded<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .expect("background panic completion deadline")
}

#[derive(Default)]
struct Woken(AtomicBool);
impl Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}
fn pending<F: Future>(mut future: Pin<&mut F>) -> Arc<Woken> {
    let woken = Arc::new(Woken::default());
    let waker = Waker::from(woken.clone());
    assert!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending(),
        "predecessor bypass"
    );
    woken
}

fn oracle(prefix: &str, expected: usize) -> Vec<Value> {
    let raw = include_str!("../../../tests/fixtures/background-panic/oracle.json");
    crate::test_baseline::oracle(raw);
    let value: Value = serde_json::from_str(raw).unwrap();
    let cases: Vec<_> = value["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["id"].as_str().unwrap().starts_with(prefix))
        .cloned()
        .collect();
    let ids: BTreeSet<_> = cases
        .iter()
        .map(|case| case["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), cases.len(), "duplicate matrix case");
    assert_eq!(cases.len(), expected, "complete matrix");
    for id in ids {
        let parts: Vec<_> = id.strip_prefix(prefix).unwrap().split('-').collect();
        if prefix == "subagent-" {
            assert!(
                [
                    "sync",
                    "poll",
                    "cancel-before",
                    "cancel-running",
                    "ok",
                    "err"
                ]
                .contains(&id.strip_prefix(prefix).unwrap()),
                "unknown matrix case"
            );
        } else {
            assert!(
                parts.len() == 3
                    && ["0", "1", "3"].contains(&parts[0])
                    && ["before", "after"].contains(&parts[1])
                    && ["sync", "poll", "err", "ok"].contains(&parts[2]),
                "unknown matrix case"
            );
        }
    }
    cases
}

struct Store {
    point: String,
    mode: String,
    gate: Mutex<Option<oneshot::Receiver<()>>>,
    calls: Mutex<Vec<String>>,
}

impl Store {
    fn operation(&self, name: String) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send>> {
        self.calls.lock().unwrap().push(name.clone());
        if name != self.point {
            return Box::pin(async { Ok(()) });
        }
        assert_ne!(self.mode, "sync", "snapshot synchronous panic");
        let gate = self.gate.lock().unwrap().take().unwrap();
        let mode = self.mode.clone();
        Box::pin(async move {
            let _ = gate.await; // Dropped predecessor also unblocks failure settlement.
            assert_ne!(mode, "poll", "snapshot poll panic");
            if mode == "err" {
                Err(io::Error::other("snapshot failure"))
            } else {
                Ok(())
            }
        })
    }
}

impl BackgroundTaskSnapshotStore for Store {
    fn load(&self) -> Vec<PersistedBackgroundTask> {
        ["a", "b"]
            .map(|id| PersistedBackgroundTask {
                task: persist_task(&BackgroundBashTask {
                    task_id: id.into(),
                    agent_id: JsValue::Undefined,
                    session_id: JsValue::Undefined,
                    kind: "bash".into(),
                    command: "".into(),
                    cwd: "/".into(),
                    pid: None,
                    status: TaskStatus::Completed,
                    exit_code: Some(Some(0)),
                    interrupted: false,
                    started_at: 0.0,
                    ended_at: Some(1.0),
                    output_bytes: 0,
                }),
                output: PersistedOutput {
                    total_bytes: 0,
                    persisted: false,
                },
            })
            .into()
    }
    fn write(
        &self,
        snapshot: PersistedBackgroundTask,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        self.operation(format!("write:{}", snapshot.task.task_id))
    }
    fn flush(&self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        self.operation("flush".into())
    }
}

#[tokio::test]
async fn snapshot_panic_matrix() {
    let cases = oracle("write-", 24).into_iter().chain(oracle("flush-", 24));
    for case in cases {
        let id = case["id"].as_str().unwrap();
        let parts: Vec<_> = id.split('-').collect();
        let (point, n, timing, mode) = (
            parts[0],
            parts[1].parse::<usize>().unwrap(),
            parts[2],
            parts[3],
        );
        let (release, gate) = oneshot::channel();
        let store = Arc::new(Store {
            point: if point == "write" { "write:a" } else { "flush" }.into(),
            mode: mode.into(),
            gate: Mutex::new(Some(gate)),
            calls: Mutex::default(),
        });
        let scratch = crate::test_scratch::Scratch::new("794-snapshot");
        let runtime = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
            disk_spill_dir: Some(scratch.path().into()),
            snapshot_store: Some(store.clone()),
            ..Default::default()
        });
        runtime.inner.starts.send_replace(1);
        let mut owner = Box::pin(runtime.dispose());
        pending(owner.as_mut());
        let mut observers = Vec::new();
        let mut wakeups = Vec::new();
        if timing == "before" {
            for _ in 0..n {
                let mut observer = Box::pin(runtime.dispose());
                wakeups.push(pending(observer.as_mut()));
                observers.push(observer);
            }
        }
        tokio::task::yield_now().await;
        assert!(
            store.calls.lock().unwrap().is_empty(),
            "{id}: predecessor bypass"
        );
        drop(owner); // Cancelling the first observer must not cancel owned disposal.
        runtime.inner.starts.send_replace(0);
        bounded(async {
            while !store.calls.lock().unwrap().contains(&store.point) {
                tokio::task::yield_now().await;
            }
        })
        .await;
        if mode != "sync" {
            assert!(
                !*runtime.inner.disposed.borrow(),
                "{id}: bypassed storage gate"
            );
        }
        drop(release); // The storage predecessor never succeeds; cancellation releases it.
        bounded(async {
            while runtime.inner.state.lock().unwrap().phase != Phase::Disposed {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            *runtime.inner.disposed.borrow(),
            "{id}: retained disposed notification"
        );
        assert!(
            wakeups.iter().all(|w| w.0.load(Ordering::SeqCst)),
            "{id}: every observer notified"
        );
        if timing == "after" {
            observers.extend((0..n).map(|_| Box::pin(runtime.dispose())));
        }
        for observer in observers {
            bounded(observer).await;
        }
        bounded(runtime.dispose()).await;
        assert_eq!(
            *store.calls.lock().unwrap(),
            ["write:a", "write:b", "flush"],
            "{id}: continue after failure"
        );
        let ts_failure = point == "flush" && mode != "ok";
        assert_eq!(
            case["outcome"],
            if ts_failure {
                "snapshot failure"
            } else {
                "fulfilled"
            }
        );
        assert_eq!(
            case["phase"],
            if ts_failure { "draining" } else { "disposed" }
        );
    }
}

#[tokio::test]
async fn subagent_panic_matrix() {
    for case in oracle("subagent-", 6) {
        let mode = case["id"]
            .as_str()
            .unwrap()
            .strip_prefix("subagent-")
            .unwrap();
        let runtime = BackgroundSubagentRuntime::default();
        let (release, gate) = oneshot::channel::<()>();
        let (entered, started) = oneshot::channel();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let run_mode = mode.to_owned();
        let task = runtime.start(super::tests::subagent_input(Box::new(move |_| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            entered.send(()).unwrap();
            assert_ne!(run_mode, "sync", "synchronous subagent panic");
            Box::pin(async move {
                let _ = gate.await;
                assert!(
                    !matches!(run_mode.as_str(), "poll" | "cancel-running"),
                    "subagent poll panic"
                );
                if run_mode == "err" {
                    Err("ordinary error".into())
                } else {
                    Ok(())
                }
            })
        })));
        let entry = runtime.find(&task.task_id).unwrap();
        let mut done = entry.done.subscribe();
        let cancelled = mode.starts_with("cancel-");
        if mode == "cancel-before" {
            assert!(runtime.stop(&task.task_id));
        } else {
            bounded(started).await.unwrap();
            if mode == "cancel-running" {
                assert!(!*done.borrow(), "unreleased subagent predecessor");
                assert!(runtime.stop(&task.task_id));
            }
        }
        let at_cancel = cancelled.then(|| runtime.get(&task.task_id).unwrap().ended_at);
        let mut observers = Vec::new();
        let mut wakeups = Vec::new();
        if cancelled {
            for _ in 0..3 {
                let mut observer = Box::pin(runtime.stop_and_wait(&task.task_id));
                wakeups.push(pending(observer.as_mut()));
                observers.push(observer);
            }
        }
        drop(release);
        bounded(done.wait_for(|done| *done)).await.unwrap();
        assert!(
            wakeups.iter().all(|w| w.0.load(Ordering::SeqCst)),
            "every subagent observer notified"
        );
        for observer in observers {
            assert!(!bounded(observer).await);
        }
        for _ in 0..3 {
            assert!(!bounded(runtime.stop_and_wait(&task.task_id)).await);
        }
        let actual = runtime.get(&task.task_id).unwrap();
        assert_eq!(actual.status.as_str(), case["status"], "{mode}");
        assert_eq!(
            actual.error.map(|s| String::from_utf16_lossy(s.units())),
            case["error"].as_str().map(str::to_owned)
        );
        assert_eq!(actual.ended_at.is_some(), case["ended"].as_bool().unwrap());
        if let Some(ended) = at_cancel {
            assert_eq!(actual.ended_at, ended, "cancel timestamp retained");
        }
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            case["calls"].as_u64().unwrap() as usize
        );
        assert_eq!(runtime.list(&SubagentListFilter::default()).len(), 1);
    }
}

#[test]
#[ignore = "requires the fixed TS checkout and Node/tsx"]
fn background_panic_regenerates_identically() {
    crate::test_baseline::regenerates_identically("background-panic");
}
