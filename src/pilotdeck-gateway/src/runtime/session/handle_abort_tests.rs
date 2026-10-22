//! Replay the real TS handle at two explicitly gated cleanup futures.
use super::*;
use crate::runtime::project::{
    ProjectRuntime, ProjectRuntimeRegistry, ResourceBundle, RuntimeResources,
};
use serde_json::{Value, json};
use std::path::Path;
use tokio::sync::Semaphore;

#[derive(Default)]
struct Trace {
    events: Vec<String>,
    session_disposed: bool,
    abort_calls: usize,
}

struct Gate {
    entered: Semaphore,
    release: Semaphore,
}
impl Gate {
    fn new() -> Self {
        Self {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }
    async fn wait(&self) {
        self.entered.add_permits(1);
        self.release.acquire().await.unwrap().forget();
    }
    async fn entered(&self) {
        self.entered.acquire().await.unwrap().forget();
    }
    fn open(&self) {
        self.release.add_permits(1);
    }
}

struct Session {
    trace: Arc<Mutex<Trace>>,
    gate: Arc<Gate>,
}
impl LiveSession for Session {
    fn snapshot(&self) -> JsValue {
        JsValue::Null
    }
    fn abort(&self, reason: &JsString) -> RuntimeResult {
        let mut trace = self.trace.lock().unwrap();
        trace.abort_calls += 1;
        let state = if trace.session_disposed {
            "disposed"
        } else {
            "live"
        };
        trace
            .events
            .push(format!("abort:{}:{state}", reason.to_utf8().unwrap()));
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async {
            self.trace
                .lock()
                .unwrap()
                .events
                .push("session_dispose_start".into());
            self.gate.wait().await;
            let mut trace = self.trace.lock().unwrap();
            trace.session_disposed = true;
            trace.events.push("session_dispose_done".into());
            Ok(())
        })
    }
}

struct LeaseResource {
    trace: Arc<Mutex<Trace>>,
    gate: Arc<Gate>,
}
impl RuntimeResources for LeaseResource {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async {
            self.trace
                .lock()
                .unwrap()
                .events
                .push("lease_release_start".into());
            self.gate.wait().await;
            self.trace
                .lock()
                .unwrap()
                .events
                .push("lease_release_done".into());
            Ok(())
        })
    }
}

fn checkpoint(
    phase: &str,
    handle: &LiveSessionHandle,
    runtime: &ProjectRuntime,
    trace: &Mutex<Trace>,
    done: bool,
) -> Value {
    let trace = trace.lock().unwrap();
    json!({
        "phase": phase,
        "state": match handle.state() {
            HandleState::Active => "active",
            HandleState::Draining => "draining",
            HandleState::Disposed => "disposed",
        },
        "done": done,
        "sessionDisposed": trace.session_disposed,
        "abortCalls": trace.abort_calls,
        "leaseCount": runtime.lease_count(),
        "runtimeState": runtime.state().as_str(),
        "events": trace.events,
    })
}

async fn record(active: bool) -> Value {
    let trace = Arc::new(Mutex::new(Trace::default()));
    let dispose_gate = Arc::new(Gate::new());
    let lease_gate = Arc::new(Gate::new());
    let registry = ProjectRuntimeRegistry::new("/virtual/abort-contract", {
        let trace = trace.clone();
        let gate = lease_gate.clone();
        Arc::new(move |_: &Path, bundle: &mut ResourceBundle| {
            bundle.add(LeaseResource {
                trace: trace.clone(),
                gate: gate.clone(),
            });
            Ok(())
        })
    });
    let lease = registry.acquire(None).unwrap();
    let runtime = lease.runtime().clone();
    registry.invalidate(None); // The final lease release must await cleanup.
    let session = Arc::new(Session {
        trace: trace.clone(),
        gate: dispose_gate.clone(),
    });
    let handle = LiveSessionHandle::new(session, Some(lease));
    let activity = active.then(|| handle.begin_activity().unwrap());
    let mut disposal = handle.dispose("dispose".into());
    drop(activity);
    dispose_gate.entered().await;
    let mut checkpoints = Vec::new();
    // Poll the real shared completion future at every checkpoint. A premature
    // completion (even without a state change) must differ from the TS trace.
    macro_rules! capture {
        ($phase:expr) => {{
            let done = std::future::poll_fn(|cx| {
                std::task::Poll::Ready(match disposal.as_mut().poll(cx) {
                    std::task::Poll::Pending => false,
                    std::task::Poll::Ready(result) => {
                        result.unwrap();
                        true
                    }
                })
            })
            .await;
            checkpoints.push(checkpoint($phase, &handle, &runtime, &trace, done));
        }};
    }
    capture!("dispose_pending");
    handle.abort(&"during-dispose".into()).unwrap();
    handle.abort(&"during-dispose".into()).unwrap();
    capture!("dispose_aborts");
    dispose_gate.open();
    lease_gate.entered().await;
    capture!("lease_pending");
    handle.abort(&"during-lease".into()).unwrap();
    handle.abort(&"during-lease".into()).unwrap();
    capture!("lease_aborts");
    lease_gate.open();
    disposal.await.unwrap();
    checkpoints.push(checkpoint("disposed", &handle, &runtime, &trace, true));
    handle.abort(&"after-disposed".into()).unwrap();
    checkpoints.push(checkpoint("late_abort", &handle, &runtime, &trace, true));
    registry.dispose().await.unwrap();
    json!({"active": active, "checkpoints": checkpoints})
}

#[tokio::test]
async fn abort_at_dispose_and_lease_windows_matches_ts() {
    const ORACLE: &str = include_str!("../../../tests/fixtures/session-abort-contract/oracle.json");
    crate::test_baseline::oracle(ORACLE);
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    for expected in oracle["cases"].as_array().unwrap() {
        let active = expected["active"].as_bool().unwrap();
        let actual = tokio::time::timeout(std::time::Duration::from_secs(3), record(active))
            .await
            .expect("controlled dispose/lease interleaving hung");
        assert_eq!(
            actual, *expected,
            "dispose/lease abort trace; active={active}"
        );
    }
}
