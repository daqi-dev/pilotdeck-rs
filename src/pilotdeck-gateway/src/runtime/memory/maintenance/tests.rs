use super::*;
use crate::runtime::refresh::*;
use pilotdeck_protocol::stringify_json;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::sync::oneshot;
type Events = Arc<Mutex<Vec<Value>>>;
fn value(v: &JsValue) -> Value {
    serde_json::from_str(&stringify_json(v).unwrap().unwrap()).unwrap()
}
fn error(e: &MemoryProviderError) -> Value {
    let mut v = json!({"name":e.name,"message":String::from_utf16_lossy(e.message.units())});
    if let Some(code) = &e.code {
        v["code"] = code.clone().into();
    }
    v
}
fn failure(name: &str, message: &str) -> MemoryProviderError {
    let mut e = MemoryProviderError::new(message);
    e.name = name.into();
    e
}
fn oracle(name: &str) -> Vec<Value> {
    let all: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/memory-maintenance/oracle.json"
    ))
    .unwrap();
    all["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap()["events"]
        .as_array()
        .unwrap()
        .clone()
}
struct Service {
    id: &'static str,
    events: Events,
    pending: Mutex<VecDeque<oneshot::Sender<Result<(), MemoryProviderError>>>>,
    panic_after: AtomicBool,
    panic_kind: &'static str,
    panic_first: AtomicBool,
    immediate: bool,
}
impl MemoryMaintenanceService for Service {
    fn run_due_scheduled_maintenance(&self, reason: &str) -> MemoryFuture<'_, ()> {
        assert!(
            !self.panic_first.swap(false, Ordering::SeqCst),
            "first poll panic"
        );
        self.events
            .lock()
            .unwrap()
            .push(json!(["call", self.id, reason]));
        if self.immediate {
            return Box::pin(async { Ok(()) });
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().push_back(tx);
        let panic_after = self.panic_after.swap(false, Ordering::SeqCst);
        Box::pin(async move {
            let result = rx.await.expect("test owns service completion");
            if panic_after {
                match self.panic_kind {
                    "panic_string" => std::panic::panic_any(String::from("after await panic")),
                    "panic_opaque" => std::panic::panic_any(17u8),
                    _ => panic!("after await panic"),
                }
            }
            result
        })
    }
}
impl Service {
    fn settle(&self, result: Result<(), MemoryProviderError>) {
        self.pending
            .lock()
            .unwrap()
            .pop_front()
            .expect("controlled predecessor exists")
            .send(result)
            .unwrap();
    }
}
struct Runtime {
    service: Option<Arc<Service>>,
}
impl MaintenanceRuntime for Runtime {
    fn project_root(&self) -> JsString {
        "/project".into()
    }
    fn memory_service(&self) -> Option<Arc<dyn MemoryMaintenanceService>> {
        self.service.clone().map(|s| s as _)
    }
}
struct Telemetry(Events);
impl MaintenanceTelemetry for Telemetry {
    fn track_feature_loop_stage(&self, input: JsValue) {
        self.0
            .lock()
            .unwrap()
            .push(json!(["success", value(&input)]));
    }
    fn track_error(&self, e: &MemoryProviderError, input: JsValue) {
        self.0
            .lock()
            .unwrap()
            .push(json!(["error", error(e), value(&input)]));
    }
}
// These unrelated refresh collaborators must never run on afterTurnCompleted.
struct Unused;
impl RefreshConfigStore for Unused {
    fn subscribe(&self, _: ConfigListener) -> Unsubscribe {
        panic!("unused")
    }
    fn reload(&self, _: &'static str) -> BoxFuture<ConfigReloadResult> {
        panic!("unused")
    }
}
impl RefreshRegistry for Unused {
    fn reload(&self) -> Result<BoxFuture<RuntimeResult>, RuntimeError> {
        panic!("unused")
    }
    fn invalidate(&self, _: Option<&JsString>) {
        panic!("unused")
    }
}
async fn drain() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}
async fn replay(name: &'static str) -> (Weak<Runtime>, Vec<Arc<Mutex<State>>>) {
    let events = Events::default();
    let make = |id| {
        Arc::new(Service {
            id,
            events: events.clone(),
            pending: Mutex::default(),
            panic_after: AtomicBool::new(name.starts_with("panic_")),
            panic_kind: name,
            panic_first: AtomicBool::new(false),
            immediate: name == "before_await",
        })
    };
    let a = make("A");
    let b = make("B");
    let old = Arc::new(Runtime {
        service: (name != "absent").then(|| a.clone()),
    });
    let weak = Arc::downgrade(&old);
    let current = Arc::new(Mutex::new(old.clone()));
    let failing = Arc::new(AtomicBool::new(false));
    let (e, c, f) = (events.clone(), current.clone(), failing.clone());
    let d = events.clone();
    let controller = Arc::new(ProjectMemoryMaintenanceController::new(
        Arc::new(move |key| {
            e.lock().unwrap().push(json!([
                "resolve",
                String::from_utf16_lossy(key.unwrap().units())
            ]));
            if f.load(Ordering::SeqCst) {
                return Err(RuntimeError::new("resolve failed").with_js_error(
                    crate::runtime::project::RuntimeJsError {
                        name: "TypeError".into(),
                        code: None,
                        message: "resolve failed".into(),
                        requires_host: false,
                    },
                ));
            }
            Ok(c.lock().unwrap().clone())
        }),
        Arc::new(Telemetry(events.clone())),
        Some(Arc::new(move |message, e| {
            d.lock().unwrap().push(json!([
                "diagnostic",
                String::from_utf16_lossy(message.units()),
                error(e)
            ]));
        })),
    ));
    let schedule = || {
        if let Err(e) = controller.schedule(Some(&"/project".into())) {
            let e = e.js_error().unwrap();
            events.lock().unwrap().push(json!(["throw",{"name":e.name,"message":String::from_utf16_lossy(e.message.units())}]));
        }
    };
    schedule();
    events.lock().unwrap().push(json!(["returned"]));
    if name == "resolver_error" {
        failing.store(true, Ordering::SeqCst);
        schedule();
        let bundle = RefreshBundle::new(RefreshOptions {
            config_store: Arc::new(Unused),
            registry: Arc::new(Unused),
            memory_maintenance: controller.clone(),
            router: Arc::new(|| None),
            project_root: "/project".into(),
            memory_diagnostics: None,
            sdk: None,
            log: Arc::new(ConsoleLog),
        });
        let e = bundle
            .after_turn_completed(TurnCompleted {
                session_key: "s".into(),
                project_key: None,
                run_id: None,
            })
            .unwrap_err();
        let e = e.js_error().unwrap();
        events.lock().unwrap().push(json!(["refresh_reject",{"name":e.name,"message":String::from_utf16_lossy(e.message.units())}]));
        failing.store(false, Ordering::SeqCst);
    }
    if [
        "coalesce",
        "before_await",
        "never",
        "cancelled",
        "reject_requested",
        "panic_requested",
    ]
    .contains(&name)
    {
        schedule();
        schedule();
    }
    if ["generations", "never"].contains(&name) {
        *current.lock().unwrap() = Arc::new(Runtime {
            service: Some(b.clone()),
        });
        schedule();
        b.settle(Ok(()));
        drain().await;
    }
    if name == "settling" {
        let (c, e) = (Arc::downgrade(&controller), events.clone());
        *controller.0.settling.lock().unwrap() = Some(Box::new(move || {
            let c = c.upgrade().unwrap();
            let state = c.0.states.lock().unwrap()[0].1.clone();
            {
                let state = state.lock().unwrap();
                assert!(!state.requested);
                assert!(state.in_flight);
            }
            e.lock().unwrap().push(json!(["settling"]));
            c.schedule(Some(&"/project".into())).unwrap();
        }));
    }
    if !["never", "absent", "before_await"].contains(&name) {
        a.settle(match name {
            "reject" | "reject_requested" => Err(failure("TypeError", "maintenance failed")),
            "cancelled" => Err(failure("AbortError", "cancelled")),
            _ => Ok(()),
        });
        drain().await;
        if name == "panic_retry" {
            assert!(
                !controller.0.states.lock().unwrap()[0]
                    .1
                    .lock()
                    .unwrap()
                    .in_flight
            );
            schedule();
        }
        if !a.pending.lock().unwrap().is_empty() {
            a.settle(Ok(()));
            drain().await;
        }
    }
    // Bounded runnable-task draining only; never await the intentionally pending A.
    drain().await;
    assert_eq!(*events.lock().unwrap(), oracle(name), "{name}");
    if name != "never" {
        assert!(a.pending.lock().unwrap().is_empty());
        assert!(
            controller
                .0
                .states
                .lock()
                .unwrap()
                .iter()
                .all(|(_, s)| !s.lock().unwrap().in_flight)
        );
    }
    let states = controller
        .0
        .states
        .lock()
        .unwrap()
        .iter()
        .map(|(_, s)| s.clone())
        .collect();
    (weak, states)
}
macro_rules! cases { ($($name:ident),*) => { $(#[test] fn $name() {
    let executor = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let (weak, states) = executor.block_on(replay(stringify!($name))); drop(executor);
    assert!(states.iter().all(|s| !s.lock().unwrap().in_flight), "cancelled worker releases flight");
    assert!(weak.upgrade().is_none(), "executor shutdown releases pending generation");
})* }; }
cases!(
    idle,
    coalesce,
    settling,
    reject,
    reject_requested,
    generations,
    absent,
    resolver_error,
    never,
    cancelled,
    before_await,
    panic_after_await,
    panic_requested,
    panic_retry,
    panic_string,
    panic_opaque
);

#[test]
fn concurrent_requests_and_first_poll_unwind() {
    let executor = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let _enter = executor.enter();
    let events = Events::default();
    let service = Arc::new(Service {
        id: "A",
        events: events.clone(),
        pending: Mutex::default(),
        panic_after: AtomicBool::new(false),
        panic_kind: "",
        panic_first: AtomicBool::new(true),
        immediate: false,
    });
    let runtime = Arc::new(Runtime {
        service: Some(service.clone()),
    });
    let controller = Arc::new(ProjectMemoryMaintenanceController::new(
        Arc::new(move |_| Ok(runtime.clone())),
        Arc::new(Telemetry(events.clone())),
        None,
    ));
    let c = controller.clone();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || c.schedule(None))).is_err()
    );
    assert!(
        !controller.0.states.lock().unwrap()[0]
            .1
            .lock()
            .unwrap()
            .in_flight
    );
    controller.schedule(None).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(8));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let (c, b) = (controller.clone(), barrier.clone());
            scope.spawn(move || {
                b.wait();
                c.schedule(None).unwrap();
            });
        }
    });
    assert_eq!(service.pending.lock().unwrap().len(), 1);
    service.settle(Ok(()));
    executor.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while service.pending.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
            service.settle(Ok(()));
            while controller.0.states.lock().unwrap()[0]
                .1
                .lock()
                .unwrap()
                .in_flight
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both controlled calls settle");
    });
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e[0] == "call")
            .count(),
        2
    );
}

#[tokio::test]
async fn cancelled_invocation_releases_flight_and_retries() {
    let events = Events::default();
    let service = Arc::new(Service {
        id: "A",
        events: events.clone(),
        pending: Mutex::default(),
        panic_after: AtomicBool::new(false),
        panic_kind: "",
        panic_first: AtomicBool::new(false),
        immediate: false,
    });
    let runtime = Arc::new(Runtime {
        service: Some(service.clone()),
    });
    let diagnostics = events.clone();
    let controller = ProjectMemoryMaintenanceController::new(
        Arc::new(move |_| Ok(runtime.clone())),
        Arc::new(Telemetry(events.clone())),
        Some(Arc::new(move |_, e| {
            diagnostics
                .lock()
                .unwrap()
                .push(json!(["diagnostic", error(e)]));
        })),
    );
    controller.0.cancel_next.store(true, Ordering::SeqCst);
    controller.schedule(None).unwrap();
    // First poll registered the pending receiver before the real task is aborted.
    assert_eq!(service.pending.lock().unwrap().len(), 1);
    drain().await;
    assert!(
        !controller.0.states.lock().unwrap()[0]
            .1
            .lock()
            .unwrap()
            .in_flight
    );
    assert!(
        service
            .pending
            .lock()
            .unwrap()
            .pop_front()
            .unwrap()
            .is_closed()
    );
    assert_eq!(
        *events.lock().unwrap(),
        vec![json!(["call", "A", "scheduled"])]
    );
    controller.schedule(None).unwrap();
    service.settle(Ok(()));
    drain().await;
    assert!(
        !controller.0.states.lock().unwrap()[0]
            .1
            .lock()
            .unwrap()
            .in_flight
    );
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            json!(["call", "A", "scheduled"]),
            json!(["call", "A", "scheduled"]),
            oracle("idle").last().unwrap().clone(),
        ]
    );
}

// ------------------------------------------------------------------ #2558
// stderr

/// The re-exec probe's env: `run:<token>`; the child also requires the token
/// as an argv positional, so a stray env value alone can never branch a
/// normal test run into the child path (the #2301 probe-token shape, like
/// the #2434 ws probe).
const DIAGNOSTIC_STDERR_PROBE_ENV: &str = "PDRS_2558_MAINTENANCE_STDERR_PROBE";
const DIAGNOSTIC_STDERR_MARKER_ENV: &str = "PDRS_2558_MAINTENANCE_STDERR_MARKER";

fn diagnostic_stderr_probe_token() -> Option<String> {
    let value = std::env::var_os(DIAGNOSTIC_STDERR_PROBE_ENV)?
        .to_str()?
        .to_owned();
    let (_, token) = value.split_once(':')?;
    if token.is_empty() {
        return None;
    }
    std::env::args()
        .any(|argument| argument == token)
        .then(|| token.to_owned())
}

/// #2558: the default diagnostic closure was an `eprintln!`, which panics
/// when the stderr write fails (std behavior). The panic would kill the
/// maintenance loop's task after it already recorded the failure it is
/// reporting — the telemetry error above it — stranding every later round,
/// while TS's `console.error` only loses the line. The fix is the shared
/// `console_log_line`, which drops the write error. This control drives the
/// default closure itself (no `diagnostic` override) with a failing
/// maintenance round, in a re-exec'd child whose stderr is a pipe with the
/// read end already dropped (every write fails with EPIPE); the child
/// installs a panic hook first, so a panicking `eprintln!` leaves a marker
/// file and exits 33 instead of passing. A read-only fd is not a failing
/// sink for this purpose: `std::io::Stderr` redirects EBADF to a fresh
/// `/dev/null` and reports success, so only EPIPE reaches the `eprintln!`
/// panic.
#[tokio::test]
async fn default_diagnostic_survives_a_failed_stderr_write() {
    use tokio::io::AsyncReadExt as _;
    if let Some(token) = diagnostic_stderr_probe_token() {
        default_diagnostic_stderr_probe_child(&token).await;
        return;
    }
    let token = format!("pdrs-2558-{}", uuid::Uuid::new_v4().simple());
    let marker = std::env::temp_dir().join(format!("{token}.marker"));
    let mut child = tokio::process::Command::new(
        std::env::current_exe().expect("#2558 the probe binary path"),
    )
    .args([
        "--exact",
        "runtime::memory::maintenance::tests::default_diagnostic_survives_a_failed_stderr_write",
        "--nocapture",
        "--test-threads=1",
        &token,
    ])
    .env(DIAGNOSTIC_STDERR_PROBE_ENV, format!("run:{token}"))
    .env(DIAGNOSTIC_STDERR_MARKER_ENV, &marker)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .expect("#2558 the failed-stderr probe child spawns");
    // The failing sink: a pipe whose read end is dropped before the child
    // writes, so every stderr write fails with EPIPE.
    drop(child.stderr.take());
    // The stdout pipe is drained concurrently with the wait, never after it
    // (the #2301 probe's drain-before-wait shape).
    let stdout = child.stdout.take();
    let console_task = tokio::spawn(async move {
        let mut out = Vec::new();
        if let Some(mut pipe) = stdout {
            let _ = pipe.read_to_end(&mut out).await;
        }
        String::from_utf8_lossy(&out).into_owned()
    });
    // Every wait states its termination condition: 30 seconds on the child,
    // then kill + reap before the named failure.
    let status = match tokio::time::timeout(std::time::Duration::from_secs(30), child.wait()).await
    {
        Ok(status) => status.expect("#2558 the failed-stderr probe child is waitable"),
        Err(_) => {
            child
                .kill()
                .await
                .expect("#2558 the timed-out probe child is killed");
            let _ = child.wait().await;
            crate::negative_assert!(
                false,
                "#2558 the failed-stderr probe child did not finish within 30s"
            );
            unreachable!("negative_assert! panics");
        }
    };
    let console = console_task
        .await
        .expect("#2558 the probe's stdout drain finishes");
    crate::negative_assert!(
        status.success(),
        "#2558 the default diagnostic survives a failed stderr write (exit {status})\n{console}"
    );
    // Positive receipt — success alone is not evidence the probe case
    // ran: a renamed test makes the `--exact` filter match nothing and
    // libtest still exits 0 (the #273 / fix-r1 lesson).
    crate::negative_assert!(
        crate::test_exact_receipt::single_pass(
            &status,
            "runtime::memory::maintenance::tests::default_diagnostic_survives_a_failed_stderr_write",
            &console,
        ),
        "#2558 the child's libtest receipt proves the probe case ran\n{console}"
    );
    // The child leaves a reached marker once the diagnostic has provably
    // run: it clears `in_flight` only after the failure report.
    let reached = std::env::temp_dir().join(format!("{token}.reached"));
    crate::negative_assert!(
        reached.exists(),
        "#2558 the child never reached the default diagnostic line"
    );
    let _ = std::fs::remove_file(&reached);
    crate::negative_assert!(
        !marker.exists(),
        "#2558 no panic marker after the default diagnostic ({}): {:?}",
        marker.display(),
        std::fs::read_to_string(&marker).unwrap_or_default()
    );
}

/// The child half of [`default_diagnostic_survives_a_failed_stderr_write`]:
/// panic hook first (marker + exit 33), then a controller constructed
/// without a diagnostic override, whose failing round must report through
/// the default closure — the shared non-panicking writer — and still clear
/// its flight. Nothing here prints to stdout: the single-threaded receipt
/// needs the whole test line intact.
async fn default_diagnostic_stderr_probe_child(token: &str) {
    let marker = std::env::var(DIAGNOSTIC_STDERR_MARKER_ENV)
        .unwrap_or_else(|_| panic!("#2558 {token} is missing the marker env"));
    std::panic::set_hook(Box::new(move |info| {
        // The default hook would print to the failed stderr; the marker is
        // the parent's evidence instead.
        let _ = std::fs::write(&marker, format!("{info}"));
        std::process::exit(33);
    }));
    let events = Events::default();
    let service = Arc::new(Service {
        id: "A",
        events: events.clone(),
        pending: Mutex::default(),
        panic_after: AtomicBool::new(false),
        panic_kind: "",
        panic_first: AtomicBool::new(false),
        immediate: false,
    });
    let runtime = Arc::new(Runtime {
        service: Some(service.clone()),
    });
    let controller = Arc::new(ProjectMemoryMaintenanceController::new(
        Arc::new(move |_| Ok(runtime.clone())),
        Arc::new(Telemetry(events.clone())),
        // No diagnostic override: the default closure under test.
        None,
    ));
    controller
        .schedule(None)
        .expect("#2558 the round is scheduled");
    assert_eq!(
        service.pending.lock().unwrap().len(),
        1,
        "#2558 the failing round is in flight"
    );
    service.settle(Err(failure("TypeError", "#2558 maintenance failed")));
    // The loop reports the failure before it clears `in_flight`, so the
    // flag clearing is the receipt that the diagnostic line already ran
    // (the same polling shape the cancelled-round test uses).
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while controller.0.states.lock().unwrap()[0]
            .1
            .lock()
            .unwrap()
            .in_flight
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#2558 the failing maintenance round settles");
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event[0] == "error"),
        "#2558 the round recorded its failure before the diagnostic"
    );
    let _ = std::fs::write(
        std::env::temp_dir().join(format!("{token}.reached")),
        b"reached",
    );
}
