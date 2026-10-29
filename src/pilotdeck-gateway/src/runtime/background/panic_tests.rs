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

const SNAPSHOT_FAMILIES: [&str; 4] = ["write-a", "write-b", "flush-empty", "flush-pair"];
const SUBAGENT_MODES: [&str; 6] = [
    "sync",
    "poll",
    "cancel-before",
    "cancel-running",
    "ok",
    "err",
];

fn oracle(id: &str) -> Value {
    let raw = include_str!("../../../tests/fixtures/background-panic/oracle.json");
    crate::test_baseline::oracle(raw);
    let value: Value = serde_json::from_str(raw).unwrap();
    let cases = value["cases"].as_array().unwrap();
    let mut expected = BTreeSet::new();
    for family in SNAPSHOT_FAMILIES {
        for n in [0, 1, 3] {
            for timing in ["before", "after"] {
                for mode in ["sync", "poll", "err", "ok"] {
                    expected.insert(format!("{family}-{n}-{timing}-{mode}"));
                }
            }
        }
    }
    expected.extend(SUBAGENT_MODES.map(|mode| format!("subagent-{mode}")));
    // Check the entire oracle before selecting a case: a foreign family must
    // not disappear behind a prefix filter.
    let ids: BTreeSet<_> = cases
        .iter()
        .map(|case| case["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids.len(), cases.len(), "duplicate matrix case");
    assert!(ids.is_subset(&expected), "unknown matrix case or family");
    assert_eq!(ids, expected, "complete matrix");
    cases.iter().find(|case| case["id"] == id).unwrap().clone()
}

struct Store {
    empty: bool,
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
        let ids: &[&str] = if self.empty { &[] } else { &["a", "b"] };
        ids.iter()
            .map(|id| PersistedBackgroundTask {
                task: persist_task(&BackgroundBashTask {
                    task_id: (*id).into(),
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
            .collect()
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

async fn snapshot_case(family: &str, n: usize, timing: &str, mode: &str) {
    let id = format!("{family}-{n}-{timing}-{mode}");
    let case = oracle(&id);
    let point = match family {
        "write-a" => "write:a",
        "write-b" => "write:b",
        "flush-empty" | "flush-pair" => "flush",
        _ => panic!("unknown snapshot family"),
    };
    let (release, gate) = oneshot::channel();
    let store = Arc::new(Store {
        empty: family == "flush-empty",
        point: point.into(),
        mode: mode.into(),
        gate: Mutex::new(Some(gate)),
        calls: Mutex::default(),
    });
    let scratch = crate::test_scratch::Scratch::new("799-snapshot");
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
        case["calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|call| call.as_str().unwrap())
            .collect::<Vec<_>>(),
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

// One test per matrix cell also lets negative controls run a single cell.
macro_rules! snapshot_family {
    ($module:ident, $family:literal) => {
        mod $module {
            use super::*;
            macro_rules! cell {
                ($name:ident, $n:literal, $timing:literal, $mode:literal) => {
                    #[tokio::test]
                    async fn $name() {
                        snapshot_case($family, $n, $timing, $mode).await;
                    }
                };
            }
            cell!(zero_before_sync, 0, "before", "sync");
            cell!(zero_before_poll, 0, "before", "poll");
            cell!(zero_before_err, 0, "before", "err");
            cell!(zero_before_ok, 0, "before", "ok");
            cell!(zero_after_sync, 0, "after", "sync");
            cell!(zero_after_poll, 0, "after", "poll");
            cell!(zero_after_err, 0, "after", "err");
            cell!(zero_after_ok, 0, "after", "ok");
            cell!(one_before_sync, 1, "before", "sync");
            cell!(one_before_poll, 1, "before", "poll");
            cell!(one_before_err, 1, "before", "err");
            cell!(one_before_ok, 1, "before", "ok");
            cell!(one_after_sync, 1, "after", "sync");
            cell!(one_after_poll, 1, "after", "poll");
            cell!(one_after_err, 1, "after", "err");
            cell!(one_after_ok, 1, "after", "ok");
            cell!(three_before_sync, 3, "before", "sync");
            cell!(three_before_poll, 3, "before", "poll");
            cell!(three_before_err, 3, "before", "err");
            cell!(three_before_ok, 3, "before", "ok");
            cell!(three_after_sync, 3, "after", "sync");
            cell!(three_after_poll, 3, "after", "poll");
            cell!(three_after_err, 3, "after", "err");
            cell!(three_after_ok, 3, "after", "ok");
        }
    };
}
snapshot_family!(snapshot_write_a, "write-a");
snapshot_family!(snapshot_write_b, "write-b");
snapshot_family!(snapshot_flush_empty, "flush-empty");
snapshot_family!(snapshot_flush_pair, "flush-pair");

async fn subagent_case(mode: &str) {
    let case = oracle(&format!("subagent-{mode}"));
    let runtime = BackgroundSubagentRuntime::default();
    let (release, gate) = oneshot::channel::<()>();
    let (entered, started) = oneshot::channel();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = calls.clone();
    let run_mode = mode.to_owned();
    let task = runtime.start(super::tests::subagent_input(Box::new(move |_| {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        entered.send(()).unwrap();
        if run_mode == "sync" {
            std::panic::panic_any("subagent synchronous failure");
        }
        Box::pin(async move {
            let _ = gate.await;
            if matches!(run_mode.as_str(), "poll" | "cancel-running") {
                std::panic::panic_any(String::from("subagent asynchronous failure"));
            }
            if run_mode == "err" {
                Err("ordinary error".into())
            } else {
                Ok(())
            }
        })
    })));
    let entry = runtime
        .find(&task.task_id)
        .unwrap_or_else(|| panic!("#818 subagent find for {mode}"));
    let mut done = entry.done.subscribe();
    let cancelled = mode.starts_with("cancel-");
    if mode == "cancel-before" {
        assert!(runtime.stop(&task.task_id));
    } else {
        bounded(started)
            .await
            .unwrap_or_else(|_| panic!("#818 subagent started for {mode}"));
        if mode == "cancel-running" {
            assert!(!*done.borrow(), "unreleased subagent predecessor");
            assert!(runtime.stop(&task.task_id));
        }
    }
    let at_cancel = cancelled.then(|| {
        runtime
            .get(&task.task_id)
            .unwrap_or_else(|| panic!("#818 subagent cancel get for {mode}"))
            .ended_at
    });
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
    bounded(done.wait_for(|done| *done))
        .await
        .unwrap_or_else(|_| panic!("#818 subagent done for {mode}"));
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
    let actual = runtime
        .get(&task.task_id)
        .unwrap_or_else(|| panic!("#818 subagent get for {mode}"));
    assert_eq!(
        actual.status.as_str(),
        case["status"],
        "#818 subagent status for {mode}"
    );
    assert_eq!(
        actual.error.map(|s| String::from_utf16_lossy(s.units())),
        case["error"].as_str().map(str::to_owned),
        "#818 subagent error for {mode}"
    );
    assert_eq!(
        actual.ended_at.is_some(),
        case["ended"]
            .as_bool()
            .unwrap_or_else(|| panic!("#818 subagent oracle ended for {mode}")),
        "#818 subagent ended for {mode}"
    );
    if let Some(ended) = at_cancel {
        assert_eq!(actual.ended_at, ended, "#818 cancel timestamp retained");
    }
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        case["calls"]
            .as_u64()
            .unwrap_or_else(|| panic!("#818 subagent oracle calls for {mode}")) as usize,
        "#818 subagent calls for {mode}"
    );
    assert_eq!(
        runtime.list(&SubagentListFilter::default()).len(),
        1,
        "#818 subagent list for {mode}"
    );
}

macro_rules! subagent_cell {
    ($name:ident, $mode:literal) => {
        #[tokio::test]
        async fn $name() {
            subagent_case($mode).await;
        }
    };
}
subagent_cell!(subagent_sync, "sync");
subagent_cell!(subagent_poll, "poll");
subagent_cell!(subagent_cancel_before, "cancel-before");
subagent_cell!(subagent_cancel_running, "cancel-running");
subagent_cell!(subagent_ok, "ok");
subagent_cell!(subagent_err, "err");

async fn non_string_panic(sync: bool) {
    let runtime = BackgroundSubagentRuntime::default();
    let mode = if sync { "sync" } else { "poll" };
    let task = runtime.start(super::tests::subagent_input(Box::new(move |_| {
        if sync {
            std::panic::panic_any(799_u32);
        }
        Box::pin(async { std::panic::panic_any(799_u32) })
    })));
    let entry = runtime
        .find(&task.task_id)
        .unwrap_or_else(|| panic!("#818 subagent non-string find for {mode}"));
    bounded(entry.done.subscribe().wait_for(|done| *done))
        .await
        .unwrap_or_else(|_| panic!("#818 subagent non-string done for {mode}"));
    let task = runtime
        .get(&task.task_id)
        .unwrap_or_else(|| panic!("#818 subagent non-string get for {mode}"));
    assert_eq!(
        task.status,
        TaskStatus::Failed,
        "#818 subagent non-string status for {mode}"
    );
    assert_eq!(
        task.error,
        Some("Background subagent task panicked.".into()),
        "#818 subagent non-string error for {mode}"
    );
    assert!(
        task.ended_at.is_some(),
        "#818 subagent non-string ended for {mode}"
    );
    assert!(
        !bounded(runtime.stop_and_wait(&task.task_id)).await,
        "#818 subagent non-string stop for {mode}"
    );
}

#[tokio::test]
async fn subagent_non_string_sync() {
    non_string_panic(true).await;
}

#[tokio::test]
async fn subagent_non_string_poll() {
    non_string_panic(false).await;
}

#[test]
fn subagent_no_panic_payload() {
    assert_eq!(
        subagent_panic_message(None),
        JsString::from("Background subagent task panicked."),
        "#818 subagent no-payload fallback"
    );
}

#[test]
#[ignore = "requires the fixed TS checkout and Node/tsx"]
fn background_panic_regenerates_identically() {
    crate::test_baseline::regenerates_identically("background-panic");
}
