//! #1233 fix-r1 (P0-1): deterministic snapshot-order tests.
//!
//! `persist_entry_static` awaits the output flush before it captures the
//! entry, so in a multi-thread runtime an older append's persist can run after
//! a newer settle's and overwrite it. The baseline has no such window (its
//! `persistEntry` has no await between capture and `snapshotStore.write`, and
//! JS is single-threaded), so these tests pin the port to the same ordering.
//!
//! Determinism does not rely on sleeps: a `GatedStore` wraps the real
//! `JsonFileBackgroundTaskSnapshotStore` and parks each non-terminal write
//! *after* the capture (arrival at `write` proves the snapshot was already
//! built) until the test releases it, so the parked snapshot is guaranteed
//! stale by the time it is forwarded.

use super::*;
use crate::runtime::background_snapshot::{
    BackgroundTaskSnapshotStore, JsonFileBackgroundTaskSnapshotStore, PersistedBackgroundTask,
};
use std::io;
use tokio::sync::oneshot;

/// A parked non-terminal write, handed to the test for release and join.
struct Park {
    status: String,
    total_bytes: u64,
    release: oneshot::Sender<()>,
    done: oneshot::Receiver<()>,
}

/// Real file store plus gates: non-terminal writes park after capture,
/// terminal writes pass through and are counted on completion.
#[derive(Clone)]
struct GatedStore {
    inner: JsonFileBackgroundTaskSnapshotStore,
    parked: Arc<Mutex<Vec<Park>>>,
    arrived: Arc<watch::Sender<usize>>,
    terminal: Arc<watch::Sender<usize>>,
}

impl GatedStore {
    fn new(inner: JsonFileBackgroundTaskSnapshotStore) -> Self {
        let (arrived, _) = watch::channel(0);
        let (terminal, _) = watch::channel(0);
        Self {
            inner,
            parked: Arc::new(Mutex::new(Vec::new())),
            arrived: Arc::new(arrived),
            terminal: Arc::new(terminal),
        }
    }
}

impl BackgroundTaskSnapshotStore for GatedStore {
    fn load(&self) -> Vec<PersistedBackgroundTask> {
        self.inner.load()
    }

    fn write(
        &self,
        snapshot: PersistedBackgroundTask,
        seq: u64,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        let terminal = matches!(
            snapshot.task.status.as_str(),
            "completed" | "failed" | "cancelled"
        );
        if !terminal {
            let (release, released) = oneshot::channel();
            let (done, completed) = oneshot::channel();
            self.parked.lock().unwrap().push(Park {
                status: snapshot.task.status.clone(),
                total_bytes: snapshot.output.total_bytes,
                release,
                done: completed,
            });
            self.arrived.send_modify(|count| *count += 1);
            let inner = self.inner.clone();
            return Box::pin(async move {
                let _ = released.await;
                let result = inner.write(snapshot, seq).await;
                let _ = done.send(());
                result
            });
        }
        let inner = self.inner.clone();
        let terminal = Arc::clone(&self.terminal);
        Box::pin(async move {
            let result = inner.write(snapshot, seq).await;
            terminal.send_modify(|count| *count += 1);
            result
        })
    }

    fn flush(&self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        self.inner.flush()
    }
}

async fn reached(counter: &mut watch::Receiver<usize>, count: usize, marker: &str) {
    counter.wait_for(|seen| *seen >= count).await.expect(marker);
}

/// What a restart (a fresh store over the same file) observes for the task.
fn restarted_entry(state: &Path, task_id: &str) -> Option<(String, u64)> {
    let restarted = JsonFileBackgroundTaskSnapshotStore::new(state.join("state.json"))
        .expect("#1233 reopen the snapshot store");
    restarted
        .load()
        .into_iter()
        .find(|entry| entry.task.task_id == task_id)
        .map(|entry| (entry.task.status, entry.output.total_bytes))
}

/// A running entry straight into the registry: a real child process is not
/// needed to order snapshot writes, and not having one removes supervisor
/// noise from the probe.
fn running_entry(state: &Path, task_id: &str) -> (JsString, Arc<Entry>) {
    let task_id = JsString::from(task_id);
    let entry = Arc::new(Entry {
        state: Mutex::new(EntryState {
            task: BackgroundBashTask {
                task_id: task_id.clone(),
                agent_id: JsValue::Undefined,
                session_id: JsValue::Undefined,
                kind: "bash".into(),
                command: "printf abcdef".into(),
                cwd: "/".into(),
                pid: None,
                status: TaskStatus::Running,
                exit_code: None,
                interrupted: false,
                started_at: 0.0,
                ended_at: None,
                output_bytes: 0,
            },
            output: OutputStore::with_spill(
                DEFAULT_MEMORY_BYTES,
                Some(output_path(state, &task_id)),
            ),
            child: None,
            persist_seq: 0,
        }),
        done: watch::channel(false).0,
        closed: watch::channel(false).0,
        never_exits: false,
    });
    (task_id, entry)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_append_persist_cannot_overwrite_the_terminal_snapshot() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let scratch = crate::test_scratch::Scratch::new("1233-order-terminal");
        let state = scratch.path().join("state");
        let gated = Arc::new(GatedStore::new(
            JsonFileBackgroundTaskSnapshotStore::new(state.join("state.json")).unwrap(),
        ));
        let runtime = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
            now: Some(Arc::new(|| 0.0)),
            snapshot_store: Some(gated.clone()),
            disk_spill_dir: Some(state.clone()),
            ..Default::default()
        });
        let (task_id, entry) = running_entry(&state, "1233-order-terminal");
        runtime
            .inner
            .state
            .lock()
            .unwrap()
            .entries
            .push((task_id, Arc::clone(&entry)));

        // The append's persist parks after capturing (running, 6 bytes).
        runtime.inner.append_output(&entry, b"abcdef");
        let mut arrivals = gated.arrived.subscribe();
        reached(
            &mut arrivals,
            1,
            "#1233 the append persist must reach the store",
        )
        .await;
        let mut parks = std::mem::take(&mut *gated.parked.lock().unwrap());
        assert_eq!(
            (parks[0].status.as_str(), parks[0].total_bytes),
            ("running", 6),
            "#1233 the parked append persist captured the running snapshot"
        );

        // Settle while the append persist is still parked.
        {
            let mut entry_state = entry.state.lock().unwrap();
            entry_state.task.status = TaskStatus::Cancelled;
            entry_state.task.ended_at = Some(0.0);
        }
        runtime.inner.settle(&entry);
        let mut terminals = gated.terminal.subscribe();
        reached(
            &mut terminals,
            1,
            "#1233 the terminal persist must not be dropped as superseded",
        )
        .await;
        assert_eq!(
            restarted_entry(&state, "1233-order-terminal"),
            Some(("cancelled".to_owned(), 6)),
            "#1233 the terminal persist lands first"
        );

        // Releasing the stale running persist must not overwrite the terminal
        // snapshot. Before the fix, this landed and rewrote the file with the
        // old running state.
        let stale = parks.remove(0);
        stale.release.send(()).unwrap();
        stale
            .done
            .await
            .expect("#1233 the stale append persist completes");
        assert_eq!(
            restarted_entry(&state, "1233-order-terminal"),
            Some(("cancelled".to_owned(), 6)),
            "#1233 a superseded append persist cannot overwrite the terminal snapshot"
        );
        drop(runtime);
    })
    .await
    .expect("#1233 persist order test exceeded 10 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_append_persist_cannot_overwrite_a_newer_append() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let scratch = crate::test_scratch::Scratch::new("1233-order-append");
        let state = scratch.path().join("state");
        let gated = Arc::new(GatedStore::new(
            JsonFileBackgroundTaskSnapshotStore::new(state.join("state.json")).unwrap(),
        ));
        let runtime = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
            now: Some(Arc::new(|| 0.0)),
            snapshot_store: Some(gated.clone()),
            disk_spill_dir: Some(state.clone()),
            ..Default::default()
        });
        let (task_id, entry) = running_entry(&state, "1233-order-append");
        runtime
            .inner
            .state
            .lock()
            .unwrap()
            .entries
            .push((task_id, Arc::clone(&entry)));

        // First append parks; only then issue the second, so the parked
        // capture is deterministically the stale one.
        runtime.inner.append_output(&entry, b"abc");
        let mut arrivals = gated.arrived.subscribe();
        reached(
            &mut arrivals,
            1,
            "#1233 the first append persist must reach the store",
        )
        .await;
        runtime.inner.append_output(&entry, b"defghi");
        reached(
            &mut arrivals,
            2,
            "#1233 the second append persist must reach the store",
        )
        .await;
        let mut parks = std::mem::take(&mut *gated.parked.lock().unwrap());
        assert_eq!(parks.len(), 2, "#1233 both append persists parked");
        // Classify by content, not arrival order.
        let (fresh, stale) = if parks[0].total_bytes == 9 {
            (parks.remove(0), parks.remove(0))
        } else {
            (parks.remove(1), parks.remove(0))
        };
        assert_eq!(
            (fresh.total_bytes, stale.total_bytes),
            (9, 3),
            "#1233 the second append persist carries the newer bytes"
        );

        fresh.release.send(()).unwrap();
        fresh
            .done
            .await
            .expect("#1233 the newer append persist completes");
        assert_eq!(
            restarted_entry(&state, "1233-order-append"),
            Some(("running".to_owned(), 9)),
            "#1233 the newer append persist lands first"
        );

        // The stale three-byte persist must be dropped, not written over the
        // nine-byte snapshot. Before the fix it landed last and won.
        stale.release.send(()).unwrap();
        stale
            .done
            .await
            .expect("#1233 the stale append persist completes");
        assert_eq!(
            restarted_entry(&state, "1233-order-append"),
            Some(("running".to_owned(), 9)),
            "#1233 a superseded append persist cannot overwrite a newer append"
        );
        drop(runtime);
    })
    .await
    .expect("#1233 persist order test exceeded 10 seconds");
}
